//! macOS shared-visibility capability boundary.
//!
//! A real three-way acceptance test must not start until all three mount
//! authorities exist. This test is deliberately rootless and non-mutating:
//! it verifies that the current FUSE API refuses macOS and records the
//! current FSKit packaging boundary before any mountpoint or backing resource
//! is created. It therefore prevents a green test run from being read as
//! proof that NFS, FUSE, and FSKit can share a location.

#![cfg(target_os = "macos")]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use mount_rs_auto::probe_transports_for;
use mount_rs_fuse::mount::{MountError as FuseMountError, MountOptions, mount as mount_fuse};
use mount_rs_memfs::MemoryFs;
use mount_rs_nfs::nfs_client_probe;

const FSKIT_CAPABILITY_BOUNDARY: &str = "FSKit has an unsigned path-backed worker checkpoint, but no containing app, installation, activation, or mounted-volume host";

#[derive(Debug, Clone, PartialEq, Eq)]
enum Capability {
    Available(String),
    Unsupported(String),
}

impl Capability {
    fn is_available(&self) -> bool {
        matches!(self, Self::Available(_))
    }

    fn reason(&self, name: &str) -> Option<String> {
        match self {
            Self::Available(_) => None,
            Self::Unsupported(reason) => Some(format!("{name}: {reason}")),
        }
    }
}

#[derive(Debug)]
struct CapabilityReport {
    nfs: Capability,
    fuse: Capability,
    fskit: Capability,
}

impl CapabilityReport {
    fn is_ready(&self) -> bool {
        self.nfs.is_available() && self.fuse.is_available() && self.fskit.is_available()
    }

    fn blockers(&self) -> Vec<String> {
        [
            ("NFS", &self.nfs),
            ("FUSE", &self.fuse),
            ("FSKit", &self.fskit),
        ]
        .into_iter()
        .filter_map(|(name, capability)| capability.reason(name))
        .collect()
    }
}

/// Paths reserved by the future three-way harness.
///
/// The mountpoints are intentionally siblings, never the shared backing
/// location itself. The lock is a separate path so an abandoned mount or a
/// provider file cannot be mistaken for the test-run ownership marker.
#[derive(Debug)]
struct SharedVisibilityPlan {
    root: PathBuf,
    backing: PathBuf,
    lock: PathBuf,
    mountpoints: [PathBuf; 3],
}

impl SharedVisibilityPlan {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "mount-rs-native-shared-{}-{nonce}",
            std::process::id()
        ));

        Self {
            backing: root.join("backing"),
            lock: root.join("run.lock"),
            mountpoints: [
                root.join("mount-nfs"),
                root.join("mount-fuse"),
                root.join("mount-fskit"),
            ],
            root,
        }
    }

    fn validate_safety(&self) -> Result<(), String> {
        let mut paths = BTreeSet::new();
        for (name, path) in [
            ("root", &self.root),
            ("backing", &self.backing),
            ("lock", &self.lock),
            ("nfs mountpoint", &self.mountpoints[0]),
            ("fuse mountpoint", &self.mountpoints[1]),
            ("fskit mountpoint", &self.mountpoints[2]),
        ] {
            if !paths.insert(path.clone()) {
                return Err(format!(
                    "{name} aliases another harness path: {}",
                    path.display()
                ));
            }
        }

        if self.backing == self.root || self.lock == self.root {
            return Err("backing and lock must not be the harness root".to_owned());
        }
        if self
            .mountpoints
            .iter()
            .any(|mountpoint| mountpoint == &self.backing || mountpoint == &self.lock)
        {
            return Err("a mountpoint aliases the backing or lock path".to_owned());
        }
        if self
            .mountpoints
            .iter()
            .any(|mountpoint| mountpoint.parent() != Some(self.root.as_path()))
        {
            return Err("mountpoints must be direct children of the harness root".to_owned());
        }

        Ok(())
    }
}

async fn current_capabilities() -> CapabilityReport {
    let host_probe = probe_transports_for("macos");
    let nfs_probe = nfs_client_probe();
    let nfs = if nfs_probe.usable {
        Capability::Available(
            "the native NFS client probe is usable; a real mount would still require explicit opt-in and cleanup"
                .to_owned(),
        )
    } else {
        Capability::Unsupported(
            nfs_probe
                .reason
                .unwrap_or_else(|| "the native NFS client probe is unavailable".to_owned()),
        )
    };

    // This call must remain a no-op on macOS. It is stronger than checking a
    // probe string: the transport API itself must refuse before validating or
    // touching a mountpoint. The deliberately nonexistent path ensures a
    // future implementation cannot accidentally mount a user directory here.
    let fuse_probe_path = std::env::temp_dir().join(format!(
        "mount-rs-fuse-capability-probe-{}",
        std::process::id()
    ));
    let fuse_result = mount_fuse(
        Arc::new(MemoryFs::empty()),
        fuse_probe_path,
        MountOptions::default(),
    )
    .await;
    let fuse_error = match fuse_result {
        Err(error @ FuseMountError::UnsupportedPlatform) => error,
        Err(error) => panic!(
            "macOS FUSE capability boundary changed: expected UnsupportedPlatform, got {error:?}"
        ),
        Ok(_) => panic!(
            "macOS FUSE unexpectedly mounted; add a real shared-visibility harness before changing this boundary test"
        ),
    };
    let fuse = Capability::Unsupported(format!(
        "{}; direct mount API returned {fuse_error:?}",
        host_probe
            .fuse
            .reason
            .unwrap_or_else(|| "the macOS FUSE probe is unavailable".to_owned())
    ));

    let fskit = Capability::Unsupported(FSKIT_CAPABILITY_BOUNDARY.to_owned());

    CapabilityReport { nfs, fuse, fskit }
}

#[tokio::test(flavor = "current_thread")]
async fn macos_three_mount_shared_visibility_fails_closed_at_capability_boundary() {
    let plan = SharedVisibilityPlan::new();
    plan.validate_safety()
        .expect("future shared-visibility plan must keep paths independent");

    let report = current_capabilities().await;
    assert!(
        !report.is_ready(),
        "three-way macOS shared visibility is not an accepted capability"
    );
    let blockers = report.blockers();
    assert!(
        blockers.iter().any(|reason| reason.starts_with("FUSE:")),
        "the FUSE blocker must remain explicit: {blockers:?}"
    );
    assert!(
        blockers.iter().any(|reason| reason.starts_with("FSKit:")),
        "the FSKit blocker must remain explicit: {blockers:?}"
    );

    eprintln!(
        "not running three-way native shared-visibility acceptance; blockers: {}",
        blockers.join("; ")
    );
}
