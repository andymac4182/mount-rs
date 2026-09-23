//! Optional, application-owned observability for mount-rs.
//!
//! The facade is intentionally outside `mount-rs-core`.  A disabled
//! [`Telemetry`] value returns futures directly, so applications that do not
//! opt in do not pay for exporter setup, path formatting, or metric updates.
//! When enabled, operation names and boundary names are static strings while
//! user-controlled paths and provider identifiers are reduced to bounded
//! shape metadata.

#![deny(unsafe_code)]

use async_trait::async_trait;
use mount_rs_core::driver::{FileHandle, FsDriver};
use mount_rs_core::error::Result;
use mount_rs_core::storage::{
    BlockId, BlockReconcileReport, BlockStore, LoadedMetadata, MetadataStore, Namespace,
    WriterLease,
};
use mount_rs_core::types::{Capabilities, DirEntry, MkdirOptions, Stats, StatsFs};
use std::collections::BTreeSet;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};
use tracing::Instrument;
use tracing::{Level, Span};

/// Versioned field/event contract for mount-rs telemetry.
pub const TELEMETRY_SCHEMA: &str = "mount-rs.telemetry.v1";
/// Stable instrumentation scope used by the OTLP providers.
pub const INSTRUMENTATION_SCOPE: &str = "mount-rs-observability";

const DEFAULT_MAX_PATH_DEPTH: usize = 16;
const MAX_SERVICE_NAME_BYTES: usize = 64;

/// Application-level telemetry settings. The default is deliberately disabled.
#[derive(Clone, Debug, PartialEq)]
pub struct TelemetryConfig {
    /// Whether spans/events/counters should be recorded.
    pub enabled: bool,
    /// OpenTelemetry `service.name` when the OTLP feature is used.
    pub service_name: String,
    /// Optional OpenTelemetry `service.version` resource attribute.
    pub service_version: Option<String>,
    /// Optional deployment environment resource attribute.
    pub environment: Option<String>,
    /// Maximum number of path segments represented in shape metadata.
    pub max_path_depth: usize,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            service_name: "mount-rs".to_owned(),
            service_version: None,
            environment: None,
            max_path_depth: DEFAULT_MAX_PATH_DEPTH,
        }
    }
}

impl TelemetryConfig {
    /// Create enabled settings for an application-owned service name.
    pub fn enabled(service_name: impl Into<String>) -> Self {
        Self {
            enabled: true,
            service_name: service_name.into(),
            ..Self::default()
        }
    }

    fn normalized(mut self) -> Self {
        self.service_name = bounded_text(&self.service_name, MAX_SERVICE_NAME_BYTES);
        if self.service_name.is_empty() {
            self.service_name = "mount-rs".to_owned();
        }
        self.service_version = self
            .service_version
            .as_deref()
            .map(|value| bounded_text(value, MAX_SERVICE_NAME_BYTES));
        self.environment = self
            .environment
            .as_deref()
            .map(|value| bounded_text(value, MAX_SERVICE_NAME_BYTES));
        self.max_path_depth = self.max_path_depth.clamp(1, DEFAULT_MAX_PATH_DEPTH);
        self
    }
}

#[derive(Debug, Default)]
struct LocalMetrics {
    operations: AtomicU64,
    successes: AtomicU64,
    errors: AtomicU64,
    duration_ms_total: AtomicU64,
    duration_ms_max: AtomicU64,
    bytes_read: AtomicU64,
    bytes_written: AtomicU64,
    reconcile_scanned: AtomicU64,
    reconcile_protected: AtomicU64,
    reconcile_recent: AtomicU64,
    reconcile_deleted: AtomicU64,
}

#[cfg(feature = "otlp")]
struct OtelMetrics {
    operations: opentelemetry::metrics::Counter<u64>,
    errors: opentelemetry::metrics::Counter<u64>,
    duration_ms: opentelemetry::metrics::Histogram<f64>,
    bytes_read: opentelemetry::metrics::Counter<u64>,
    bytes_written: opentelemetry::metrics::Counter<u64>,
    reconcile_scanned: opentelemetry::metrics::Counter<u64>,
    reconcile_protected: opentelemetry::metrics::Counter<u64>,
    reconcile_recent: opentelemetry::metrics::Counter<u64>,
    reconcile_deleted: opentelemetry::metrics::Counter<u64>,
}

struct TelemetryState {
    config: TelemetryConfig,
    metrics: LocalMetrics,
    #[cfg(feature = "otlp")]
    otel_metrics: OtelMetrics,
}

/// A clonable, cheap handle used by wrappers and application boundaries.
#[derive(Clone)]
pub struct Telemetry {
    state: Option<Arc<TelemetryState>>,
}

impl std::fmt::Debug for Telemetry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Telemetry")
            .field("enabled", &self.is_enabled())
            .field(
                "service_name",
                &self
                    .state
                    .as_ref()
                    .map(|state| state.config.service_name.as_str()),
            )
            .finish()
    }
}

impl Default for Telemetry {
    fn default() -> Self {
        Self::disabled()
    }
}

impl Telemetry {
    /// Return a no-op handle. This is the default for every boundary.
    pub fn disabled() -> Self {
        Self { state: None }
    }

    /// Construct an enabled handle. Exporter setup remains application-owned.
    pub fn new(config: TelemetryConfig) -> Self {
        let config = config.normalized();
        if !config.enabled {
            return Self::disabled();
        }

        #[cfg(feature = "otlp")]
        let otel_metrics = {
            let meter = opentelemetry::global::meter(INSTRUMENTATION_SCOPE);
            OtelMetrics {
                operations: meter.u64_counter("mount_rs.operations").build(),
                errors: meter.u64_counter("mount_rs.errors").build(),
                duration_ms: meter
                    .f64_histogram("mount_rs.operation.duration_ms")
                    .build(),
                bytes_read: meter.u64_counter("mount_rs.bytes.read").build(),
                bytes_written: meter.u64_counter("mount_rs.bytes.written").build(),
                reconcile_scanned: meter
                    .u64_counter("mount_rs.blocks.reconcile.scanned")
                    .build(),
                reconcile_protected: meter
                    .u64_counter("mount_rs.blocks.reconcile.protected")
                    .build(),
                reconcile_recent: meter
                    .u64_counter("mount_rs.blocks.reconcile.recent")
                    .build(),
                reconcile_deleted: meter
                    .u64_counter("mount_rs.blocks.reconcile.deleted")
                    .build(),
            }
        };

        Self {
            state: Some(Arc::new(TelemetryState {
                config,
                metrics: LocalMetrics::default(),
                #[cfg(feature = "otlp")]
                otel_metrics,
            })),
        }
    }

    /// Construct from `MOUNT_RS_TELEMETRY=1` without installing an exporter.
    /// This is useful for applications that install their own subscriber.
    pub fn from_env(service_name: impl Into<String>) -> Self {
        let enabled = std::env::var("MOUNT_RS_TELEMETRY")
            .ok()
            .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes"));
        Self::new(TelemetryConfig {
            enabled,
            service_name: service_name.into(),
            ..TelemetryConfig::default()
        })
    }

    pub fn is_enabled(&self) -> bool {
        self.state.is_some()
    }

    pub fn config(&self) -> Option<&TelemetryConfig> {
        self.state.as_ref().map(|state| &state.config)
    }

    /// Start a bounded operation span. No user-controlled value is used as a
    /// span name or metric label.
    pub fn span(
        &self,
        boundary: &'static str,
        operation: &'static str,
        path: Option<&str>,
    ) -> Span {
        let Some(state) = &self.state else {
            return Span::none();
        };
        let summary = summarize_path_with_limit(path, state.config.max_path_depth);
        tracing::info_span!(
            target: "mount_rs.operation",
            "mount_rs.operation",
            telemetry_schema = TELEMETRY_SCHEMA,
            boundary = boundary,
            operation = operation,
            path_rooted = summary.rooted,
            path_depth = summary.depth,
            path_size_bucket = summary.size_bucket,
        )
    }

    /// Start an operation span with an extracted OpenTelemetry parent. When
    /// no tracing/OpenTelemetry subscriber is installed this remains a safe
    /// no-op, just like [`Self::span`].
    #[cfg(feature = "otlp")]
    pub fn span_with_context(
        &self,
        boundary: &'static str,
        operation: &'static str,
        path: Option<&str>,
        parent: &opentelemetry::Context,
    ) -> Span {
        let span = self.span(boundary, operation, path);
        use tracing_opentelemetry::OpenTelemetrySpanExt;
        let _ = span.set_parent(parent.clone());
        span
    }

    /// Instrument a future whose failure is a mount-rs [`FsError`].
    pub async fn observe_fs<T, F>(
        &self,
        boundary: &'static str,
        operation: &'static str,
        path: Option<&str>,
        future: F,
    ) -> Result<T>
    where
        F: Future<Output = Result<T>> + Send,
    {
        self.observe_result(boundary, operation, path, future, |error| {
            Some(error.code.as_str())
        })
        .await
    }

    /// Instrument a future with a bounded, caller-supplied error class.
    /// Callers must return a finite/static label; backend messages are never
    /// accepted here.
    pub async fn observe_result<T, E, F, C>(
        &self,
        boundary: &'static str,
        operation: &'static str,
        path: Option<&str>,
        future: F,
        classify_error: C,
    ) -> std::result::Result<T, E>
    where
        F: Future<Output = std::result::Result<T, E>> + Send,
        C: FnOnce(&E) -> Option<&'static str> + Send,
    {
        if !self.is_enabled() {
            return future.await;
        }
        let started = Instant::now();
        let result = future
            .instrument(self.span(boundary, operation, path))
            .await;
        let error_code = result.as_ref().err().and_then(classify_error);
        self.record_outcome(boundary, operation, started.elapsed(), error_code);
        result
    }

    /// Observe a result while preserving an extracted OpenTelemetry parent
    /// across the async future.
    #[cfg(feature = "otlp")]
    pub async fn observe_result_with_context<T, E, F, C>(
        &self,
        boundary: &'static str,
        operation: &'static str,
        path: Option<&str>,
        parent: &opentelemetry::Context,
        future: F,
        classify_error: C,
    ) -> std::result::Result<T, E>
    where
        F: Future<Output = std::result::Result<T, E>> + Send,
        C: FnOnce(&E) -> Option<&'static str> + Send,
    {
        if !self.is_enabled() {
            return future.await;
        }
        let started = Instant::now();
        let result = future
            .instrument(self.span_with_context(boundary, operation, path, parent))
            .await;
        let error_code = result.as_ref().err().and_then(classify_error);
        self.record_outcome(boundary, operation, started.elapsed(), error_code);
        result
    }

    /// Instrument a boundary where the result itself is already encoded as a
    /// response and therefore has no `Result` error type.
    pub async fn in_span<T, F>(
        &self,
        boundary: &'static str,
        operation: &'static str,
        path: Option<&str>,
        future: F,
    ) -> T
    where
        F: Future<Output = T> + Send,
    {
        if !self.is_enabled() {
            return future.await;
        }
        future
            .instrument(self.span(boundary, operation, path))
            .await
    }

    /// Record bytes on the current operation without creating a high-cardinality label.
    pub fn record_bytes(&self, operation: &'static str, bytes: u64) {
        let Some(state) = &self.state else {
            return;
        };
        match operation {
            "read" => {
                state.metrics.bytes_read.fetch_add(bytes, Ordering::Relaxed);
                #[cfg(feature = "otlp")]
                state.otel_metrics.bytes_read.add(bytes, &[]);
            }
            "write" => {
                state
                    .metrics
                    .bytes_written
                    .fetch_add(bytes, Ordering::Relaxed);
                #[cfg(feature = "otlp")]
                state.otel_metrics.bytes_written.add(bytes, &[]);
            }
            _ => {}
        }
    }

    /// Record bounded orphan-reconciliation counts without exposing object
    /// keys, provider messages, or other high-cardinality values.
    pub fn record_reconcile(&self, report: &BlockReconcileReport) {
        let Some(state) = &self.state else {
            return;
        };
        state
            .metrics
            .reconcile_scanned
            .fetch_add(report.scanned, Ordering::Relaxed);
        state
            .metrics
            .reconcile_protected
            .fetch_add(report.protected, Ordering::Relaxed);
        state
            .metrics
            .reconcile_recent
            .fetch_add(report.recent, Ordering::Relaxed);
        state
            .metrics
            .reconcile_deleted
            .fetch_add(report.deleted, Ordering::Relaxed);
        tracing::event!(
            target: "mount_rs.event",
            Level::INFO,
            telemetry_schema = TELEMETRY_SCHEMA,
            event_name = "mount_rs.blocks.reconciled",
            boundary = "provider.blocks",
            operation = "reconcile",
            scanned = report.scanned,
            protected = report.protected,
            recent = report.recent,
            deleted = report.deleted,
        );
        #[cfg(feature = "otlp")]
        {
            use opentelemetry::KeyValue;
            let attributes = [
                KeyValue::new("mount_rs.boundary", "provider.blocks"),
                KeyValue::new("mount_rs.operation", "reconcile"),
            ];
            state
                .otel_metrics
                .reconcile_scanned
                .add(report.scanned, &attributes);
            state
                .otel_metrics
                .reconcile_protected
                .add(report.protected, &attributes);
            state
                .otel_metrics
                .reconcile_recent
                .add(report.recent, &attributes);
            state
                .otel_metrics
                .reconcile_deleted
                .add(report.deleted, &attributes);
        }
    }

    fn record_outcome(
        &self,
        boundary: &'static str,
        operation: &'static str,
        elapsed: Duration,
        error_code: Option<&'static str>,
    ) {
        let Some(state) = &self.state else {
            return;
        };
        let duration_ms = elapsed.as_millis().min(u64::MAX as u128) as u64;
        state.metrics.operations.fetch_add(1, Ordering::Relaxed);
        state
            .metrics
            .duration_ms_total
            .fetch_add(duration_ms, Ordering::Relaxed);
        update_max(&state.metrics.duration_ms_max, duration_ms);
        let outcome = if error_code.is_some() { "error" } else { "ok" };
        if error_code.is_some() {
            state.metrics.errors.fetch_add(1, Ordering::Relaxed);
        } else {
            state.metrics.successes.fetch_add(1, Ordering::Relaxed);
        }

        let error_code = error_code.unwrap_or("none");
        if error_code == "none" {
            tracing::event!(
                target: "mount_rs.event",
                Level::DEBUG,
                telemetry_schema = TELEMETRY_SCHEMA,
                event_name = "mount_rs.operation.finished",
                boundary = boundary,
                operation = operation,
                outcome = outcome,
                error_code = error_code,
                duration_ms = duration_ms,
            );
        } else {
            tracing::event!(
                target: "mount_rs.event",
                Level::WARN,
                telemetry_schema = TELEMETRY_SCHEMA,
                event_name = "mount_rs.operation.finished",
                boundary = boundary,
                operation = operation,
                outcome = outcome,
                error_code = error_code,
                duration_ms = duration_ms,
            );
        }

        #[cfg(feature = "otlp")]
        {
            use opentelemetry::KeyValue;
            let attributes = [
                KeyValue::new("mount_rs.boundary", boundary),
                KeyValue::new("mount_rs.operation", operation),
                KeyValue::new("mount_rs.outcome", outcome),
                KeyValue::new("mount_rs.error_code", error_code),
            ];
            state.otel_metrics.operations.add(1, &attributes);
            state
                .otel_metrics
                .duration_ms
                .record(duration_ms as f64, &attributes);
            if error_code != "none" {
                state.otel_metrics.errors.add(1, &attributes);
            }
        }
    }

    /// Return a point-in-time local snapshot for deterministic tests and
    /// health endpoints. OTLP export is owned by the configured SDK provider.
    pub fn snapshot(&self) -> MetricsSnapshot {
        let Some(state) = &self.state else {
            return MetricsSnapshot::default();
        };
        MetricsSnapshot {
            operations: state.metrics.operations.load(Ordering::Relaxed),
            successes: state.metrics.successes.load(Ordering::Relaxed),
            errors: state.metrics.errors.load(Ordering::Relaxed),
            duration_ms_total: state.metrics.duration_ms_total.load(Ordering::Relaxed),
            duration_ms_max: state.metrics.duration_ms_max.load(Ordering::Relaxed),
            bytes_read: state.metrics.bytes_read.load(Ordering::Relaxed),
            bytes_written: state.metrics.bytes_written.load(Ordering::Relaxed),
            reconcile_scanned: state.metrics.reconcile_scanned.load(Ordering::Relaxed),
            reconcile_protected: state.metrics.reconcile_protected.load(Ordering::Relaxed),
            reconcile_recent: state.metrics.reconcile_recent.load(Ordering::Relaxed),
            reconcile_deleted: state.metrics.reconcile_deleted.load(Ordering::Relaxed),
        }
    }
}

/// Bounded local metrics useful for deterministic tests and diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MetricsSnapshot {
    pub operations: u64,
    pub successes: u64,
    pub errors: u64,
    pub duration_ms_total: u64,
    pub duration_ms_max: u64,
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub reconcile_scanned: u64,
    pub reconcile_protected: u64,
    pub reconcile_recent: u64,
    pub reconcile_deleted: u64,
}

fn update_max(value: &AtomicU64, candidate: u64) {
    let mut current = value.load(Ordering::Relaxed);
    while candidate > current {
        match value.compare_exchange_weak(current, candidate, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
}

fn bounded_text(value: &str, max_bytes: usize) -> String {
    let mut bounded = String::new();
    for character in value.chars() {
        if bounded.len() + character.len_utf8() > max_bytes {
            break;
        }
        bounded.push(character);
    }
    bounded
}

/// Shape-only path metadata. It intentionally contains no path value or hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathSummary {
    pub rooted: bool,
    pub depth: u16,
    pub size_bucket: u8,
}

pub fn summarize_path(path: Option<&str>) -> PathSummary {
    summarize_path_with_limit(path, DEFAULT_MAX_PATH_DEPTH)
}

fn summarize_path_with_limit(path: Option<&str>, max_depth: usize) -> PathSummary {
    let Some(path) = path else {
        return PathSummary {
            rooted: false,
            depth: 0,
            size_bucket: 0,
        };
    };
    let depth = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .count()
        .min(max_depth)
        .min(u16::MAX as usize) as u16;
    let size_bucket = match path.len() {
        0 => 0,
        1..=16 => 1,
        17..=64 => 2,
        65..=256 => 3,
        257..=1024 => 4,
        _ => 5,
    };
    PathSummary {
        rooted: path.starts_with('/'),
        depth,
        size_bucket,
    }
}

static GLOBAL: OnceLock<RwLock<Telemetry>> = OnceLock::new();

fn global_cell() -> &'static RwLock<Telemetry> {
    GLOBAL.get_or_init(|| RwLock::new(Telemetry::disabled()))
}

/// Read the application-wide handle used by boundary integrations.
pub fn global() -> Telemetry {
    global_cell()
        .read()
        .map(|telemetry| telemetry.clone())
        .unwrap_or_else(|_| Telemetry::disabled())
}

/// Replace the application-wide handle. Call this during application startup,
/// before constructing long-lived sessions or drivers.
pub fn set_global(telemetry: Telemetry) {
    if let Ok(mut current) = global_cell().write() {
        *current = telemetry;
    }
}

/// Driver decorator that adds core-operation spans, finite error events, and
/// byte counters without changing the core trait or its dependency graph.
pub struct InstrumentedDriver {
    inner: Arc<dyn FsDriver>,
    telemetry: Telemetry,
}

impl std::fmt::Debug for InstrumentedDriver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InstrumentedDriver")
            .field("telemetry", &self.telemetry)
            .finish_non_exhaustive()
    }
}

impl InstrumentedDriver {
    pub fn new<D>(driver: D, telemetry: Telemetry) -> Self
    where
        D: FsDriver + 'static,
    {
        Self::from_arc(Arc::new(driver), telemetry)
    }

    pub fn from_arc(driver: Arc<dyn FsDriver>, telemetry: Telemetry) -> Self {
        Self {
            inner: driver,
            telemetry,
        }
    }

    pub fn inner(&self) -> Arc<dyn FsDriver> {
        Arc::clone(&self.inner)
    }

    pub fn into_arc(self) -> Arc<dyn FsDriver> {
        Arc::new(self)
    }
}

#[async_trait]
impl FsDriver for InstrumentedDriver {
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    fn supports_guarded_mutations(&self) -> bool {
        self.inner.supports_guarded_mutations()
    }

    fn supports_guarded_reads(&self) -> bool {
        self.inner.supports_guarded_reads()
    }

    fn stable_inode_ids(&self) -> bool {
        self.inner.stable_inode_ids()
    }

    async fn guarded_mutation(
        &self,
        request: mount_rs_core::GuardedMutation,
    ) -> Result<mount_rs_core::GuardedMutationResult> {
        let path = self.telemetry.is_enabled().then(|| {
            match &request {
                mount_rs_core::GuardedMutation::Setattr { target, .. } => &target.path,
                mount_rs_core::GuardedMutation::Open { parent, .. }
                | mount_rs_core::GuardedMutation::Mkdir { parent, .. }
                | mount_rs_core::GuardedMutation::Symlink { parent, .. }
                | mount_rs_core::GuardedMutation::Mknod { parent, .. }
                | mount_rs_core::GuardedMutation::Unlink { parent, .. }
                | mount_rs_core::GuardedMutation::Rmdir { parent, .. } => &parent.path,
                mount_rs_core::GuardedMutation::Rename { from_parent, .. } => &from_parent.path,
                mount_rs_core::GuardedMutation::Link { source, .. } => &source.path,
            }
            .to_owned()
        });
        let result = self
            .telemetry
            .observe_fs(
                "core",
                "guarded_mutation",
                path.as_deref(),
                self.inner.guarded_mutation(request),
            )
            .await?;
        Ok(match result {
            mount_rs_core::GuardedMutationResult::Opened { handle, identity } => {
                mount_rs_core::GuardedMutationResult::Opened {
                    handle: Arc::new(InstrumentedFileHandle {
                        inner: handle,
                        telemetry: self.telemetry.clone(),
                    }),
                    identity,
                }
            }
            other => other,
        })
    }

    async fn guarded_read(
        &self,
        request: mount_rs_core::GuardedRead,
    ) -> Result<mount_rs_core::GuardedReadResult> {
        let path = self.telemetry.is_enabled().then(|| {
            match &request {
                mount_rs_core::GuardedRead::Stat { target }
                | mount_rs_core::GuardedRead::Readlink { target } => &target.path,
                mount_rs_core::GuardedRead::Lookup { parent, .. } => &parent.path,
                mount_rs_core::GuardedRead::Readdir { directory, .. } => &directory.path,
            }
            .to_owned()
        });
        self.telemetry
            .observe_fs(
                "core",
                "guarded_read",
                path.as_deref(),
                self.inner.guarded_read(request),
            )
            .await
    }

    async fn syncfs(&self) -> Result<()> {
        self.telemetry
            .observe_fs("core", "syncfs", None, self.inner.syncfs())
            .await
    }

    async fn stat(&self, path: &str) -> Result<Stats> {
        self.telemetry
            .observe_fs("core", "stat", Some(path), self.inner.stat(path))
            .await
    }

    async fn lstat(&self, path: &str) -> Result<Stats> {
        self.telemetry
            .observe_fs("core", "lstat", Some(path), self.inner.lstat(path))
            .await
    }

    async fn statfs(&self, path: &str) -> Result<StatsFs> {
        self.telemetry
            .observe_fs("core", "statfs", Some(path), self.inner.statfs(path))
            .await
    }

    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>> {
        self.telemetry
            .observe_fs("core", "readdir", Some(path), self.inner.readdir(path))
            .await
    }

    async fn readdir_bounded(&self, path: &str, max_entries: usize) -> Result<Vec<DirEntry>> {
        self.telemetry
            .observe_fs(
                "core",
                "readdir_bounded",
                Some(path),
                self.inner.readdir_bounded(path, max_entries),
            )
            .await
    }

    async fn open(&self, path: &str, flags: &str, mode: u32) -> Result<Arc<dyn FileHandle>> {
        let result = self
            .telemetry
            .observe_fs(
                "core",
                "open",
                Some(path),
                self.inner.open(path, flags, mode),
            )
            .await?;
        Ok(Arc::new(InstrumentedFileHandle {
            inner: result,
            telemetry: self.telemetry.clone(),
        }))
    }

    async fn open_flags(
        &self,
        path: &str,
        flags: mount_rs_core::OpenFlags,
        mode: u32,
    ) -> Result<Arc<dyn FileHandle>> {
        let result = self
            .telemetry
            .observe_fs(
                "core",
                "open_flags",
                Some(path),
                self.inner.open_flags(path, flags, mode),
            )
            .await?;
        Ok(Arc::new(InstrumentedFileHandle {
            inner: result,
            telemetry: self.telemetry.clone(),
        }))
    }

    async fn write_file(&self, path: &str, data: &[u8]) -> Result<()> {
        self.telemetry
            .observe_fs(
                "core",
                "write_file",
                Some(path),
                self.inner.write_file(path, data),
            )
            .await
    }

    async fn mkdir(&self, path: &str, options: MkdirOptions) -> Result<Option<String>> {
        self.telemetry
            .observe_fs("core", "mkdir", Some(path), self.inner.mkdir(path, options))
            .await
    }

    async fn rmdir(&self, path: &str) -> Result<()> {
        self.telemetry
            .observe_fs("core", "rmdir", Some(path), self.inner.rmdir(path))
            .await
    }

    async fn unlink(&self, path: &str) -> Result<()> {
        self.telemetry
            .observe_fs("core", "unlink", Some(path), self.inner.unlink(path))
            .await
    }

    async fn rename(&self, old_path: &str, new_path: &str) -> Result<()> {
        self.telemetry
            .observe_fs(
                "core",
                "rename",
                Some(old_path),
                self.inner.rename(old_path, new_path),
            )
            .await
    }

    async fn link(&self, existing_path: &str, new_path: &str) -> Result<()> {
        self.telemetry
            .observe_fs(
                "core",
                "link",
                Some(existing_path),
                self.inner.link(existing_path, new_path),
            )
            .await
    }

    async fn symlink(&self, target: &str, path: &str) -> Result<()> {
        self.telemetry
            .observe_fs(
                "core",
                "symlink",
                Some(path),
                self.inner.symlink(target, path),
            )
            .await
    }

    async fn readlink(&self, path: &str) -> Result<String> {
        self.telemetry
            .observe_fs("core", "readlink", Some(path), self.inner.readlink(path))
            .await
    }

    async fn chmod(&self, path: &str, mode: u32) -> Result<()> {
        self.telemetry
            .observe_fs("core", "chmod", Some(path), self.inner.chmod(path, mode))
            .await
    }

    async fn chown(&self, path: &str, uid: u32, gid: u32) -> Result<()> {
        self.telemetry
            .observe_fs(
                "core",
                "chown",
                Some(path),
                self.inner.chown(path, uid, gid),
            )
            .await
    }

    async fn lchown(&self, path: &str, uid: u32, gid: u32) -> Result<()> {
        self.telemetry
            .observe_fs(
                "core",
                "lchown",
                Some(path),
                self.inner.lchown(path, uid, gid),
            )
            .await
    }

    async fn truncate(&self, path: &str, length: u64) -> Result<()> {
        self.telemetry
            .observe_fs(
                "core",
                "truncate",
                Some(path),
                self.inner.truncate(path, length),
            )
            .await
    }

    fn has_utimens(&self) -> bool {
        self.inner.has_utimens()
    }

    async fn utimens(
        &self,
        path: &str,
        atime_ns: i128,
        mtime_ns: i128,
        follow_symlinks: bool,
    ) -> Result<()> {
        self.telemetry
            .observe_fs(
                "core",
                "utimens",
                Some(path),
                self.inner
                    .utimens(path, atime_ns, mtime_ns, follow_symlinks),
            )
            .await
    }

    async fn utimes(&self, path: &str, atime_ms: i64, mtime_ms: i64) -> Result<()> {
        self.telemetry
            .observe_fs(
                "core",
                "utimes",
                Some(path),
                self.inner.utimes(path, atime_ms, mtime_ms),
            )
            .await
    }

    async fn lutimes(&self, path: &str, atime_ms: i64, mtime_ms: i64) -> Result<()> {
        self.telemetry
            .observe_fs(
                "core",
                "lutimes",
                Some(path),
                self.inner.lutimes(path, atime_ms, mtime_ms),
            )
            .await
    }

    async fn mknod(&self, path: &str, mode: u32, dev: u64) -> Result<()> {
        self.telemetry
            .observe_fs(
                "core",
                "mknod",
                Some(path),
                self.inner.mknod(path, mode, dev),
            )
            .await
    }
}

struct InstrumentedFileHandle {
    inner: Arc<dyn FileHandle>,
    telemetry: Telemetry,
}

#[async_trait]
impl FileHandle for InstrumentedFileHandle {
    fn fd(&self) -> Option<u64> {
        self.inner.fd()
    }

    async fn read(&self, buffer: &mut [u8], position: Option<u64>) -> Result<usize> {
        let result = self
            .telemetry
            .observe_fs(
                "core.handle",
                "read",
                None,
                self.inner.read(buffer, position),
            )
            .await?;
        self.telemetry.record_bytes("read", result as u64);
        Ok(result)
    }

    async fn write(&self, buffer: &[u8], position: Option<u64>) -> Result<usize> {
        let result = self
            .telemetry
            .observe_fs(
                "core.handle",
                "write",
                None,
                self.inner.write(buffer, position),
            )
            .await?;
        self.telemetry.record_bytes("write", result as u64);
        Ok(result)
    }

    async fn stat(&self) -> Result<Stats> {
        self.telemetry
            .observe_fs("core.handle", "stat", None, self.inner.stat())
            .await
    }

    async fn truncate(&self, length: u64) -> Result<()> {
        self.telemetry
            .observe_fs("core.handle", "truncate", None, self.inner.truncate(length))
            .await
    }

    async fn sync(&self) -> Result<()> {
        self.telemetry
            .observe_fs("core.handle", "sync", None, self.inner.sync())
            .await
    }

    async fn datasync(&self) -> Result<()> {
        self.telemetry
            .observe_fs("core.handle", "datasync", None, self.inner.datasync())
            .await
    }

    async fn close(&self) -> Result<()> {
        self.telemetry
            .observe_fs("core.handle", "close", None, self.inner.close())
            .await
    }
}

/// Decorate a metadata provider without changing `mount-rs-core`'s provider
/// traits or adding exporter dependencies to provider crates.
pub struct InstrumentedMetadataStore<M> {
    inner: Arc<M>,
    telemetry: Telemetry,
}

impl<M> InstrumentedMetadataStore<M> {
    pub fn new(inner: M, telemetry: Telemetry) -> Self {
        Self {
            inner: Arc::new(inner),
            telemetry,
        }
    }

    pub fn from_arc(inner: Arc<M>, telemetry: Telemetry) -> Self {
        Self { inner, telemetry }
    }

    pub fn inner(&self) -> Arc<M> {
        Arc::clone(&self.inner)
    }
}

#[async_trait]
impl<M> MetadataStore for InstrumentedMetadataStore<M>
where
    M: MetadataStore + 'static,
{
    fn durable(&self) -> bool {
        self.inner.durable()
    }

    fn publish_includes_flush_barrier(&self) -> bool {
        self.inner.publish_includes_flush_barrier()
    }

    async fn load(&self) -> Result<LoadedMetadata> {
        self.telemetry
            .observe_fs("provider.metadata", "load", None, self.inner.load())
            .await
    }

    async fn prepare_concurrent_mode(&self) -> Result<()> {
        self.telemetry
            .observe_fs(
                "provider.metadata",
                "concurrent.prepare",
                None,
                self.inner.prepare_concurrent_mode(),
            )
            .await
    }

    async fn acquire_writer(&self, owner: &str, ttl: Duration) -> Result<WriterLease> {
        self.telemetry
            .observe_fs(
                "provider.metadata",
                "lease.acquire",
                None,
                self.inner.acquire_writer(owner, ttl),
            )
            .await
    }

    async fn renew_writer(&self, lease: &WriterLease, ttl: Duration) -> Result<WriterLease> {
        self.telemetry
            .observe_fs(
                "provider.metadata",
                "lease.renew",
                None,
                self.inner.renew_writer(lease, ttl),
            )
            .await
    }

    async fn release_writer(&self, lease: &WriterLease) -> Result<()> {
        self.telemetry
            .observe_fs(
                "provider.metadata",
                "lease.release",
                None,
                self.inner.release_writer(lease),
            )
            .await
    }

    async fn publish(
        &self,
        expected_revision: u64,
        lease: &WriterLease,
        namespace: Namespace,
    ) -> Result<u64> {
        self.telemetry
            .observe_fs(
                "provider.metadata",
                "publish",
                None,
                self.inner.publish(expected_revision, lease, namespace),
            )
            .await
    }

    async fn publish_if_revision(
        &self,
        expected_revision: u64,
        namespace: Namespace,
    ) -> Result<u64> {
        self.telemetry
            .observe_fs(
                "provider.metadata",
                "concurrent.publish",
                None,
                self.inner.publish_if_revision(expected_revision, namespace),
            )
            .await
    }

    async fn flush(&self) -> Result<()> {
        self.telemetry
            .observe_fs("provider.metadata", "flush", None, self.inner.flush())
            .await
    }
}

/// Decorate an immutable block provider. Block IDs and payload bytes are
/// deliberately not emitted; only operation and byte-count metrics are kept.
pub struct InstrumentedBlockStore<B> {
    inner: Arc<B>,
    telemetry: Telemetry,
}

impl<B> InstrumentedBlockStore<B> {
    pub fn new(inner: B, telemetry: Telemetry) -> Self {
        Self {
            inner: Arc::new(inner),
            telemetry,
        }
    }

    pub fn from_arc(inner: Arc<B>, telemetry: Telemetry) -> Self {
        Self { inner, telemetry }
    }

    pub fn inner(&self) -> Arc<B> {
        Arc::clone(&self.inner)
    }
}

#[async_trait]
impl<B> BlockStore for InstrumentedBlockStore<B>
where
    B: BlockStore + 'static,
{
    fn durable(&self) -> bool {
        self.inner.durable()
    }

    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        let count = bytes.len() as u64;
        let result = self
            .telemetry
            .observe_fs("provider.blocks", "put", None, self.inner.put(bytes))
            .await?;
        self.telemetry.record_bytes("write", count);
        Ok(result)
    }

    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        let result = self
            .telemetry
            .observe_fs("provider.blocks", "get", None, self.inner.get(id))
            .await?;
        self.telemetry.record_bytes("read", result.len() as u64);
        Ok(result)
    }

    async fn flush(&self) -> Result<()> {
        self.telemetry
            .observe_fs("provider.blocks", "flush", None, self.inner.flush())
            .await
    }

    async fn delete(&self, id: &BlockId) -> Result<()> {
        self.telemetry
            .observe_fs("provider.blocks", "delete", None, self.inner.delete(id))
            .await
    }

    async fn reconcile(
        &self,
        live: &BTreeSet<BlockId>,
        grace: Duration,
    ) -> Result<BlockReconcileReport> {
        let report = self
            .telemetry
            .observe_fs(
                "provider.blocks",
                "reconcile",
                None,
                self.inner.reconcile(live, grace),
            )
            .await?;
        self.telemetry.record_reconcile(&report);
        Ok(report)
    }
}

#[cfg(feature = "otlp")]
mod otlp {
    use super::{INSTRUMENTATION_SCOPE, Telemetry, TelemetryConfig, set_global};
    use opentelemetry::KeyValue;
    use opentelemetry::global;
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::logs::{
        BatchConfigBuilder as LogBatchConfigBuilder, BatchLogProcessor, SdkLoggerProvider,
    };
    use opentelemetry_sdk::metrics::SdkMeterProvider;
    use opentelemetry_sdk::trace::{
        BatchConfigBuilder as TraceBatchConfigBuilder, BatchSpanProcessor, Sampler,
        SdkTracerProvider,
    };
    use opentelemetry_sdk::{Resource, propagation::TraceContextPropagator};
    use std::time::Duration;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    /// OTLP/HTTP setup owned by the application. The endpoint is the OTLP
    /// HTTP base URL, normally `http://127.0.0.1:4318`; the exporter appends
    /// the signal paths.
    #[derive(Clone, Debug)]
    pub struct OtlpConfig {
        pub endpoint: String,
        pub service_name: String,
        pub service_version: Option<String>,
        pub environment: Option<String>,
        pub sample_ratio: f64,
        pub export_timeout: Duration,
    }

    impl Default for OtlpConfig {
        fn default() -> Self {
            Self {
                endpoint: "http://127.0.0.1:4318".to_owned(),
                service_name: "mount-rs".to_owned(),
                service_version: None,
                environment: None,
                sample_ratio: 1.0,
                export_timeout: Duration::from_secs(10),
            }
        }
    }

    #[derive(Debug)]
    pub enum OtlpError {
        Exporter(String),
        Subscriber(String),
        Shutdown(String),
    }

    impl std::fmt::Display for OtlpError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Exporter(message) => {
                    write!(formatter, "OTLP exporter setup failed: {message}")
                }
                Self::Subscriber(message) => {
                    write!(formatter, "telemetry subscriber setup failed: {message}")
                }
                Self::Shutdown(message) => {
                    write!(formatter, "telemetry shutdown failed: {message}")
                }
            }
        }
    }

    impl std::error::Error for OtlpError {}

    /// Owns the three SDK providers and flushes them on explicit shutdown.
    pub struct OtlpGuard {
        tracer_provider: SdkTracerProvider,
        meter_provider: SdkMeterProvider,
        logger_provider: SdkLoggerProvider,
    }

    impl std::fmt::Debug for OtlpGuard {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("OtlpGuard { providers: [trace, metrics, logs] }")
        }
    }

    impl OtlpGuard {
        /// Flush and stop all three providers. Export failures are returned to
        /// the owning application, while operation execution never sees them.
        pub fn shutdown(self) -> Result<(), OtlpError> {
            let mut errors = Vec::new();
            if let Err(error) = self.tracer_provider.shutdown() {
                errors.push(format!("traces: {error}"));
            }
            if let Err(error) = self.meter_provider.shutdown() {
                errors.push(format!("metrics: {error}"));
            }
            if let Err(error) = self.logger_provider.shutdown() {
                errors.push(format!("logs: {error}"));
            }
            if errors.is_empty() {
                Ok(())
            } else {
                Err(OtlpError::Shutdown(errors.join("; ")))
            }
        }

        pub fn force_flush(&self) -> Result<(), OtlpError> {
            let mut errors = Vec::new();
            if let Err(error) = self.tracer_provider.force_flush() {
                errors.push(format!("traces: {error}"));
            }
            if let Err(error) = self.meter_provider.force_flush() {
                errors.push(format!("metrics: {error}"));
            }
            if let Err(error) = self.logger_provider.force_flush() {
                errors.push(format!("logs: {error}"));
            }
            if errors.is_empty() {
                Ok(())
            } else {
                Err(OtlpError::Shutdown(errors.join("; ")))
            }
        }
    }

    /// Install an OTLP/HTTP subscriber and make an enabled [`Telemetry`]
    /// handle available to boundary integrations. Applications may instead
    /// construct their own providers and subscriber and only call
    /// [`set_global`].
    pub fn install_otlp(config: OtlpConfig) -> Result<OtlpGuard, OtlpError> {
        const MAX_QUEUE_SIZE: usize = 2048;
        const MAX_EXPORT_BATCH_SIZE: usize = 512;
        const EXPORT_INTERVAL: Duration = Duration::from_secs(5);

        let sample_ratio = if config.sample_ratio.is_finite() {
            config.sample_ratio.clamp(0.0, 1.0)
        } else {
            1.0
        };
        let telemetry_config = TelemetryConfig {
            enabled: true,
            service_name: config.service_name,
            service_version: config.service_version,
            environment: config.environment,
            ..TelemetryConfig::default()
        }
        .normalized();
        let mut resource_builder =
            Resource::builder().with_service_name(telemetry_config.service_name.clone());
        let mut resource_attributes = Vec::new();
        if let Some(version) = &telemetry_config.service_version {
            resource_attributes.push(KeyValue::new("service.version", version.clone()));
        }
        if let Some(environment) = &telemetry_config.environment {
            resource_attributes.push(KeyValue::new(
                "deployment.environment.name",
                environment.clone(),
            ));
        }
        resource_builder = resource_builder.with_attributes(resource_attributes);
        let resource = resource_builder.build();

        let span_exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_endpoint(signal_endpoint(&config.endpoint, "traces"))
            .with_timeout(config.export_timeout)
            .build()
            .map_err(|error| OtlpError::Exporter(format!("traces: {error}")))?;
        let metric_exporter = opentelemetry_otlp::MetricExporter::builder()
            .with_http()
            .with_endpoint(signal_endpoint(&config.endpoint, "metrics"))
            .with_timeout(config.export_timeout)
            .build()
            .map_err(|error| OtlpError::Exporter(format!("metrics: {error}")))?;
        let log_exporter = opentelemetry_otlp::LogExporter::builder()
            .with_http()
            .with_endpoint(signal_endpoint(&config.endpoint, "logs"))
            .with_timeout(config.export_timeout)
            .build()
            .map_err(|error| OtlpError::Exporter(format!("logs: {error}")))?;

        let trace_batch_config = TraceBatchConfigBuilder::default()
            .with_max_queue_size(MAX_QUEUE_SIZE)
            .with_max_export_batch_size(MAX_EXPORT_BATCH_SIZE)
            .with_scheduled_delay(EXPORT_INTERVAL)
            .build();
        let tracer_provider = SdkTracerProvider::builder()
            .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
                sample_ratio,
            ))))
            .with_span_processor(
                BatchSpanProcessor::builder(span_exporter)
                    .with_batch_config(trace_batch_config)
                    .build(),
            )
            .with_resource(resource.clone())
            .build();
        let tracer = tracer_provider.tracer(INSTRUMENTATION_SCOPE);
        let meter_provider = SdkMeterProvider::builder()
            .with_periodic_exporter(metric_exporter)
            .with_resource(resource.clone())
            .build();
        let log_batch_config = LogBatchConfigBuilder::default()
            .with_max_queue_size(MAX_QUEUE_SIZE)
            .with_max_export_batch_size(MAX_EXPORT_BATCH_SIZE)
            .with_scheduled_delay(EXPORT_INTERVAL)
            .build();
        let logger_provider = SdkLoggerProvider::builder()
            .with_log_processor(
                BatchLogProcessor::builder(log_exporter)
                    .with_batch_config(log_batch_config)
                    .build(),
            )
            .with_resource(resource)
            .build();

        let trace_layer = tracing_opentelemetry::layer().with_tracer(tracer);
        let log_layer = opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge::new(
            &logger_provider,
        );
        let fmt_layer = tracing_subscriber::fmt::layer()
            .json()
            .with_target(false)
            .with_current_span(true);
        tracing_subscriber::registry()
            .with(trace_layer)
            .with(log_layer)
            .with(fmt_layer)
            .try_init()
            .map_err(|error| OtlpError::Subscriber(error.to_string()))?;

        global::set_meter_provider(meter_provider.clone());
        global::set_text_map_propagator(TraceContextPropagator::new());
        set_global(Telemetry::new(telemetry_config));
        Ok(OtlpGuard {
            tracer_provider,
            meter_provider,
            logger_provider,
        })
    }

    pub use opentelemetry::{
        Context,
        propagation::{Extractor, Injector, TextMapPropagator},
    };

    /// Inject the current W3C trace context into an arbitrary carrier.
    pub fn inject_context<I: Injector>(injector: &mut I) {
        global::get_text_map_propagator(|propagator| propagator.inject(injector));
    }

    /// Extract a W3C trace context from an arbitrary carrier.
    pub fn extract_context<E: Extractor>(extractor: &E) -> Context {
        global::get_text_map_propagator(|propagator| propagator.extract(extractor))
    }

    fn signal_endpoint(base: &str, signal: &str) -> String {
        format!("{}/v1/{signal}", base.trim_end_matches('/'))
    }
}

#[cfg(feature = "otlp")]
pub use otlp::{
    Context, Extractor, Injector, OtlpConfig, OtlpError, OtlpGuard, extract_context,
    inject_context, install_otlp,
};

#[cfg(feature = "http-propagation")]
mod http_propagation {
    use http::HeaderMap;
    use opentelemetry::propagation::{Extractor, Injector};

    pub struct HeaderInjector<'a>(pub &'a mut HeaderMap);

    impl Injector for HeaderInjector<'_> {
        fn set(&mut self, key: &str, value: String) {
            if let (Ok(name), Ok(value)) = (
                http::header::HeaderName::try_from(key),
                http::header::HeaderValue::try_from(value),
            ) {
                self.0.insert(name, value);
            }
        }
    }

    pub struct HeaderExtractor<'a>(pub &'a HeaderMap);

    impl Extractor for HeaderExtractor<'_> {
        fn get(&self, key: &str) -> Option<&str> {
            self.0.get(key).and_then(|value| value.to_str().ok())
        }

        fn keys(&self) -> Vec<&str> {
            self.0
                .keys()
                .filter_map(|key| key.as_str().into())
                .collect()
        }
    }

    pub fn inject_headers(headers: &mut HeaderMap) {
        super::inject_context(&mut HeaderInjector(headers));
    }

    pub fn extract_headers(headers: &HeaderMap) -> super::Context {
        super::extract_context(&HeaderExtractor(headers))
    }
}

#[cfg(feature = "http-propagation")]
pub use http_propagation::{HeaderExtractor, HeaderInjector, extract_headers, inject_headers};

#[cfg(test)]
mod tests {
    use super::*;
    use mount_rs_core::{
        ErrorCode, FsDriver, GuardedMutation, GuardedMutationResult, GuardedRead,
        GuardedReadResult, GuardedSetattr, ObservedEntry, OpenFlags, PathGuard, PathIdentity,
    };
    use mount_rs_memfs::{MemoryFs, MemoryOptions};

    #[test]
    fn disabled_telemetry_has_zero_snapshot_and_redacts_path_shape() {
        let telemetry = Telemetry::disabled();
        assert!(!telemetry.is_enabled());
        assert_eq!(telemetry.snapshot(), MetricsSnapshot::default());
        assert_eq!(
            summarize_path(Some("/secret/tenant/a.txt")),
            PathSummary {
                rooted: true,
                depth: 3,
                size_bucket: 2,
            }
        );
    }

    #[test]
    fn enabled_telemetry_records_bounded_reconciliation_counts() {
        let telemetry = Telemetry::new(TelemetryConfig::enabled("reconcile-test"));
        telemetry.record_reconcile(&BlockReconcileReport {
            scanned: 11,
            protected: 7,
            recent: 3,
            deleted: 1,
        });
        let snapshot = telemetry.snapshot();
        assert_eq!(snapshot.reconcile_scanned, 11);
        assert_eq!(snapshot.reconcile_protected, 7);
        assert_eq!(snapshot.reconcile_recent, 3);
        assert_eq!(snapshot.reconcile_deleted, 1);
    }

    #[tokio::test]
    async fn enabled_driver_records_operations_and_bytes_without_path_values() {
        let telemetry = Telemetry::new(TelemetryConfig::enabled("test-service"));
        let driver =
            InstrumentedDriver::new(MemoryFs::new(MemoryOptions::default()), telemetry.clone());
        driver
            .open("/redacted", "w", 0o666)
            .await
            .unwrap()
            .write(b"hello", Some(0))
            .await
            .unwrap();
        assert_eq!(telemetry.snapshot().bytes_written, 5);
        assert_eq!(telemetry.snapshot().errors, 0);
        assert!(telemetry.snapshot().operations >= 2);
    }

    #[tokio::test]
    async fn instrumented_driver_forwards_guarded_open_and_observes_its_handle() {
        let telemetry = Telemetry::new(TelemetryConfig::enabled("guarded-test"));
        let backing = MemoryFs::empty();
        let root = PathIdentity::from_stats(&backing.stat("/").await.unwrap()).unwrap();
        let driver = InstrumentedDriver::new(backing, telemetry.clone());

        assert!(driver.supports_guarded_mutations());
        assert!(driver.stable_inode_ids());
        let opened = driver
            .guarded_mutation(GuardedMutation::Open {
                parent: PathGuard {
                    path: "/".into(),
                    identity: root,
                },
                name: "file".into(),
                observed: ObservedEntry::Absent,
                flags: OpenFlags::parse("wx", "/file").unwrap(),
                mode: 0o600,
            })
            .await
            .unwrap();
        let GuardedMutationResult::Opened { handle, identity } = opened else {
            panic!("guarded open must return a file handle");
        };
        assert_eq!(handle.write(b"hello", Some(0)).await.unwrap(), 5);
        handle.close().await.unwrap();
        assert_eq!(
            PathIdentity::from_stats(&driver.stat("/file").await.unwrap()),
            Some(identity)
        );
        assert_eq!(telemetry.snapshot().bytes_written, 5);
    }

    #[tokio::test]
    async fn instrumented_driver_preserves_guarded_stale_error_and_records_it() {
        let telemetry = Telemetry::new(TelemetryConfig::enabled("guarded-error-test"));
        let backing = MemoryFs::empty();
        backing
            .open("/file", "wx", 0o600)
            .await
            .unwrap()
            .close()
            .await
            .unwrap();
        let original = PathIdentity::from_stats(&backing.lstat("/file").await.unwrap()).unwrap();
        backing.unlink("/file").await.unwrap();
        backing
            .open("/file", "wx", 0o600)
            .await
            .unwrap()
            .close()
            .await
            .unwrap();
        let driver = InstrumentedDriver::new(backing, telemetry.clone());

        let error = driver
            .guarded_mutation(GuardedMutation::Setattr {
                target: PathGuard {
                    path: "/file".into(),
                    identity: original,
                },
                change: GuardedSetattr {
                    mode: Some(0o777),
                    ..GuardedSetattr::default()
                },
            })
            .await
            .err()
            .expect("replaced file must fail guarded mutation");
        assert_eq!(error.code, ErrorCode::Estale);
        assert_eq!(driver.lstat("/file").await.unwrap().mode & 0o777, 0o600);
        assert_eq!(telemetry.snapshot().errors, 1);
    }

    #[tokio::test]
    async fn instrumented_driver_forwards_guarded_reads_and_records_stale_error() {
        let telemetry = Telemetry::new(TelemetryConfig::enabled("guarded-read-test"));
        let backing = MemoryFs::empty();
        backing
            .open("/file", "wx", 0o600)
            .await
            .unwrap()
            .close()
            .await
            .unwrap();
        let root = backing.stat("/").await.unwrap();
        let file = backing.lstat("/file").await.unwrap();
        let driver = InstrumentedDriver::new(backing, telemetry.clone());

        assert!(driver.supports_guarded_reads());
        let result = driver
            .guarded_read(GuardedRead::Readdir {
                directory: PathGuard {
                    path: "/".into(),
                    identity: PathIdentity::from_stats(&root).unwrap(),
                },
                max_entries: 8,
            })
            .await
            .unwrap();
        let GuardedReadResult::Directory { stats, entries } = result else {
            panic!("guarded readdir must return directory and child attributes");
        };
        assert_eq!(stats.ino, root.ino);
        assert!(
            entries
                .iter()
                .any(|entry| entry.name == "file" && entry.stats.ino == file.ino)
        );
        assert_eq!(telemetry.snapshot().operations, 1);

        driver.unlink("/file").await.unwrap();
        driver
            .open("/file", "wx", 0o600)
            .await
            .unwrap()
            .close()
            .await
            .unwrap();
        let error = driver
            .guarded_read(GuardedRead::Stat {
                target: PathGuard {
                    path: "/file".into(),
                    identity: PathIdentity::from_stats(&file).unwrap(),
                },
            })
            .await
            .expect_err("replaced file must fail guarded stat");
        assert_eq!(error.code, ErrorCode::Estale);
        assert_eq!(telemetry.snapshot().errors, 1);
    }

    #[test]
    fn service_and_environment_values_are_bounded() {
        let config = TelemetryConfig {
            enabled: true,
            service_name: "x".repeat(200),
            service_version: Some("v".repeat(200)),
            environment: Some("e".repeat(200)),
            max_path_depth: 999,
        }
        .normalized();
        assert_eq!(config.service_name.chars().count(), MAX_SERVICE_NAME_BYTES);
        assert_eq!(
            config.service_version.unwrap().chars().count(),
            MAX_SERVICE_NAME_BYTES
        );
        assert_eq!(
            config.environment.unwrap().chars().count(),
            MAX_SERVICE_NAME_BYTES
        );
        assert_eq!(config.max_path_depth, DEFAULT_MAX_PATH_DEPTH);
    }

    #[cfg(feature = "otlp")]
    #[test]
    fn hash_map_w3c_carrier_round_trips_through_global_propagator() {
        use opentelemetry::propagation::TextMapPropagator;
        use opentelemetry::trace::{
            SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState,
        };
        use opentelemetry_sdk::propagation::TraceContextPropagator;
        use std::collections::HashMap;

        let propagator = TraceContextPropagator::new();
        let span_context = SpanContext::new(
            TraceId::from_bytes([1; 16]),
            SpanId::from_bytes([2; 8]),
            TraceFlags::SAMPLED,
            false,
            TraceState::default(),
        );
        let context = Context::new().with_remote_span_context(span_context);
        let mut carrier = HashMap::new();
        propagator.inject_context(&context, &mut carrier);
        let extracted = propagator.extract(&carrier);
        assert_eq!(
            extracted.span().span_context().trace_id(),
            TraceId::from_bytes([1; 16])
        );
        assert_eq!(carrier.len(), 2);
    }

    #[test]
    fn global_handle_can_be_replaced() {
        set_global(Telemetry::disabled());
        assert!(!global().is_enabled());
        set_global(Telemetry::new(TelemetryConfig::enabled("global-test")));
        assert!(global().is_enabled());
        set_global(Telemetry::disabled());
    }
}
