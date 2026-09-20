//! Request watcher used by the command-line mount process.
//!
//! This mirrors the pinned mountx watcher at the driver boundary. It keeps
//! metadata polls quiet unless `--verbose` is selected, always reports failed
//! operations, and wraps returned file handles so handle I/O is narrated too.

use std::future::Future;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use mount_rs_core::{
    Capabilities, DirEntry, FileHandle, FsDriver, MkdirOptions, OpenFlags, Result, Stats, StatsFs,
};

use crate::color::{CYAN, Color, DIM, GREEN, RED, YELLOW};

const NOISY: [&str; 4] = ["lstat", "stat", "statfs", "fstat"];

pub type WatchLogger = Arc<dyn Fn(String) + Send + Sync + 'static>;

#[derive(Clone)]
pub struct WatchOptions {
    pub verbose: bool,
    pub color: Color,
    pub enabled: bool,
    logger: Option<WatchLogger>,
}

impl WatchOptions {
    pub const fn new(verbose: bool, color: Color, enabled: bool) -> Self {
        Self {
            verbose,
            color,
            enabled,
            logger: None,
        }
    }

    pub fn with_logger(mut self, logger: WatchLogger) -> Self {
        self.logger = Some(logger);
        self
    }
}

impl Default for WatchOptions {
    fn default() -> Self {
        Self::new(false, Color::from_env(), true)
    }
}

#[derive(Clone)]
pub struct WatchedDriver {
    inner: Arc<dyn FsDriver>,
    options: WatchOptions,
}

pub fn watch_driver(driver: Arc<dyn FsDriver>, options: WatchOptions) -> WatchedDriver {
    WatchedDriver {
        inner: driver,
        options,
    }
}

impl WatchedDriver {
    fn emit(&self, operation: &str, subject: &str, note: &str) {
        emit_line(&self.options, operation, subject, note);
    }

    async fn watched<T, F>(
        &self,
        operation: &str,
        subject: String,
        note: String,
        future: F,
    ) -> Result<T>
    where
        F: Future<Output = Result<T>>,
    {
        let result = future.await;
        match result {
            Ok(value) => {
                if self.options.enabled && (self.options.verbose || !NOISY.contains(&operation)) {
                    self.emit(operation, &subject, &note);
                }
                Ok(value)
            }
            Err(error) => {
                self.emit(
                    operation,
                    &subject,
                    &format!("{} {}", self.options.color.red("→"), error.code.as_str()),
                );
                Err(error)
            }
        }
    }

    fn subject(operation: &str, first: &str, second: Option<&str>) -> String {
        if matches!(operation, "rename" | "link" | "symlink") {
            format!(
                "{} {} {}",
                first,
                Color::disabled().paint(DIM, "→"),
                second.unwrap_or_default()
            )
        } else {
            first.to_owned()
        }
    }
}

fn emit_line(options: &WatchOptions, operation: &str, subject: &str, note: &str) {
    if !options.enabled {
        return;
    }
    let style = operation_style(operation);
    let operation = options.color.paint(style, format!("{operation:<9}"));
    let note = if note.is_empty() {
        String::new()
    } else {
        format!("  {}", options.color.dim(note))
    };
    let line = format!(
        "{}  {operation} {subject}{note}",
        options.color.dim(timestamp())
    );
    if let Some(logger) = &options.logger {
        logger(line);
    } else {
        eprintln!("{line}");
    }
}

#[derive(Clone)]
struct WatchedHandle {
    inner: Arc<dyn FileHandle>,
    path: String,
    options: WatchOptions,
}

impl WatchedHandle {
    fn emit(&self, operation: &str, note: &str) {
        emit_line(&self.options, operation, &self.path, note);
    }

    async fn watched<T, F>(&self, operation: &str, note: String, future: F) -> Result<T>
    where
        F: Future<Output = Result<T>>,
    {
        let result = future.await;
        match result {
            Ok(value) => {
                if self.options.enabled && (self.options.verbose || !NOISY.contains(&operation)) {
                    self.emit(operation, &note);
                }
                Ok(value)
            }
            Err(error) => {
                self.emit(
                    operation,
                    &format!("{} {}", self.options.color.red("→"), error.code.as_str()),
                );
                Err(error)
            }
        }
    }
}

#[async_trait]
impl FileHandle for WatchedHandle {
    fn fd(&self) -> Option<u64> {
        self.inner.fd()
    }

    async fn read(&self, buffer: &mut [u8], position: Option<u64>) -> Result<usize> {
        let requested = buffer.len();
        self.watched(
            "read",
            format!("{requested} B"),
            self.inner.read(buffer, position),
        )
        .await
    }

    async fn write(&self, buffer: &[u8], position: Option<u64>) -> Result<usize> {
        let requested = buffer.len();
        self.watched(
            "write",
            format!("{requested} B"),
            self.inner.write(buffer, position),
        )
        .await
    }

    async fn stat(&self) -> Result<Stats> {
        self.watched("stat", String::new(), self.inner.stat()).await
    }

    async fn truncate(&self, length: u64) -> Result<()> {
        self.watched(
            "truncate",
            format!("{length} B"),
            self.inner.truncate(length),
        )
        .await
    }

    async fn sync(&self) -> Result<()> {
        self.watched("sync", String::new(), self.inner.sync()).await
    }

    async fn datasync(&self) -> Result<()> {
        self.watched("datasync", String::new(), self.inner.datasync())
            .await
    }

    async fn close(&self) -> Result<()> {
        self.watched("close", String::new(), self.inner.close())
            .await
    }
}

#[async_trait]
impl FsDriver for WatchedDriver {
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    async fn syncfs(&self) -> Result<()> {
        self.watched("syncfs", "/".to_owned(), String::new(), self.inner.syncfs())
            .await
    }

    async fn stat(&self, path: &str) -> Result<Stats> {
        self.watched(
            "stat",
            path.to_owned(),
            String::new(),
            self.inner.stat(path),
        )
        .await
    }

    async fn lstat(&self, path: &str) -> Result<Stats> {
        self.watched(
            "lstat",
            path.to_owned(),
            String::new(),
            self.inner.lstat(path),
        )
        .await
    }

    async fn statfs(&self, path: &str) -> Result<StatsFs> {
        self.watched(
            "statfs",
            path.to_owned(),
            String::new(),
            self.inner.statfs(path),
        )
        .await
    }

    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>> {
        self.watched(
            "readdir",
            path.to_owned(),
            String::new(),
            self.inner.readdir(path),
        )
        .await
    }

    async fn open(&self, path: &str, flags: &str, mode: u32) -> Result<Arc<dyn FileHandle>> {
        let handle = self
            .watched(
                "open",
                path.to_owned(),
                OpenFlags::parse(flags, path)
                    .map(flag_text)
                    .unwrap_or_else(|_| flags.to_owned()),
                self.inner.open(path, flags, mode),
            )
            .await?;
        Ok(Arc::new(WatchedHandle {
            inner: handle,
            path: path.to_owned(),
            options: self.options.clone(),
        }))
    }

    async fn open_flags(
        &self,
        path: &str,
        flags: OpenFlags,
        mode: u32,
    ) -> Result<Arc<dyn FileHandle>> {
        let handle = self
            .watched(
                "open",
                path.to_owned(),
                flag_text(flags),
                self.inner.open_flags(path, flags, mode),
            )
            .await?;
        Ok(Arc::new(WatchedHandle {
            inner: handle,
            path: path.to_owned(),
            options: self.options.clone(),
        }))
    }

    async fn mkdir(&self, path: &str, options: MkdirOptions) -> Result<Option<String>> {
        self.watched(
            "mkdir",
            path.to_owned(),
            if options.recursive {
                "recursive".to_owned()
            } else {
                String::new()
            },
            self.inner.mkdir(path, options),
        )
        .await
    }

    async fn rmdir(&self, path: &str) -> Result<()> {
        self.watched(
            "rmdir",
            path.to_owned(),
            String::new(),
            self.inner.rmdir(path),
        )
        .await
    }

    async fn unlink(&self, path: &str) -> Result<()> {
        self.watched(
            "unlink",
            path.to_owned(),
            String::new(),
            self.inner.unlink(path),
        )
        .await
    }

    async fn rename(&self, old_path: &str, new_path: &str) -> Result<()> {
        self.watched(
            "rename",
            Self::subject("rename", old_path, Some(new_path)),
            String::new(),
            self.inner.rename(old_path, new_path),
        )
        .await
    }

    async fn link(&self, existing_path: &str, new_path: &str) -> Result<()> {
        self.watched(
            "link",
            Self::subject("link", existing_path, Some(new_path)),
            String::new(),
            self.inner.link(existing_path, new_path),
        )
        .await
    }

    async fn symlink(&self, target: &str, path: &str) -> Result<()> {
        self.watched(
            "symlink",
            Self::subject("symlink", target, Some(path)),
            String::new(),
            self.inner.symlink(target, path),
        )
        .await
    }

    async fn readlink(&self, path: &str) -> Result<String> {
        self.watched(
            "readlink",
            path.to_owned(),
            String::new(),
            self.inner.readlink(path),
        )
        .await
    }

    async fn chmod(&self, path: &str, mode: u32) -> Result<()> {
        self.watched(
            "chmod",
            path.to_owned(),
            format!("0{mode:o}"),
            self.inner.chmod(path, mode),
        )
        .await
    }

    async fn chown(&self, path: &str, uid: u32, gid: u32) -> Result<()> {
        self.watched(
            "chown",
            path.to_owned(),
            format!("{uid}:{gid}"),
            self.inner.chown(path, uid, gid),
        )
        .await
    }

    async fn lchown(&self, path: &str, uid: u32, gid: u32) -> Result<()> {
        self.watched(
            "lchown",
            path.to_owned(),
            format!("{uid}:{gid}"),
            self.inner.lchown(path, uid, gid),
        )
        .await
    }

    async fn truncate(&self, path: &str, length: u64) -> Result<()> {
        self.watched(
            "truncate",
            path.to_owned(),
            format!("{length} B"),
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
        self.watched(
            "utimens",
            path.to_owned(),
            String::new(),
            self.inner
                .utimens(path, atime_ns, mtime_ns, follow_symlinks),
        )
        .await
    }

    async fn utimes(&self, path: &str, atime_ms: i64, mtime_ms: i64) -> Result<()> {
        self.watched(
            "utimes",
            path.to_owned(),
            String::new(),
            self.inner.utimes(path, atime_ms, mtime_ms),
        )
        .await
    }

    async fn lutimes(&self, path: &str, atime_ms: i64, mtime_ms: i64) -> Result<()> {
        self.watched(
            "lutimes",
            path.to_owned(),
            String::new(),
            self.inner.lutimes(path, atime_ms, mtime_ms),
        )
        .await
    }

    async fn mknod(&self, path: &str, mode: u32, dev: u64) -> Result<()> {
        self.watched(
            "mknod",
            path.to_owned(),
            String::new(),
            self.inner.mknod(path, mode, dev),
        )
        .await
    }
}

fn operation_style(operation: &str) -> u8 {
    match operation {
        "mkdir" | "symlink" | "link" | "write" => GREEN,
        "open" => CYAN,
        "unlink" | "rmdir" | "truncate" => RED,
        "rename" | "chmod" | "chown" | "lchown" | "utimes" | "lutimes" | "utimens" => YELLOW,
        _ => DIM,
    }
}

fn flag_text(flags: OpenFlags) -> String {
    let access = match (flags.read, flags.write) {
        (true, true) => "rw",
        (false, true) => "w",
        _ => "r",
    };
    let mut parts = vec![access.to_owned()];
    if flags.create {
        parts.push("create".to_owned());
    }
    if flags.exclusive {
        parts.push("excl".to_owned());
    }
    if flags.truncate {
        parts.push("trunc".to_owned());
    }
    if flags.append {
        parts.push("append".to_owned());
    }
    parts.join(",")
}

fn timestamp() -> String {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let millis = elapsed.subsec_millis();
    let seconds = elapsed.as_secs() % 86_400;
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}.{millis:03}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use mount_rs_core::MemoryFs;
    use std::sync::Mutex;

    #[tokio::test]
    async fn watcher_logs_mutations_and_suppresses_successful_metadata_polls() {
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let logger = {
            let lines = Arc::clone(&lines);
            Arc::new(move |line: String| lines.lock().unwrap().push(line)) as WatchLogger
        };
        let driver = watch_driver(
            Arc::new(MemoryFs::empty()),
            WatchOptions::new(false, Color::disabled(), true).with_logger(logger),
        );
        driver.stat("/").await.unwrap();
        let handle = driver.open("/note", "w", 0o644).await.unwrap();
        handle.write(b"hello", Some(0)).await.unwrap();
        handle.close().await.unwrap();
        let lines = lines.lock().unwrap();
        assert!(lines.iter().any(|line| line.contains("open")));
        assert!(lines.iter().any(|line| line.contains("write")));
        assert!(!lines.iter().any(|line| line.contains(" stat ")));
    }

    #[test]
    fn linux_wire_flags_are_rendered_without_host_constants() {
        let flags = OpenFlags::from_bits(0o100 | 0o200 | 0o1000 | 0o2000 | 1);
        assert_eq!(flag_text(flags), "w,create,excl,trunc,append");
    }
}
