//! Explicit Linux-only native mount smoke harness.
//!
//! This example never chooses a machine path on its own: pass an existing,
//! empty directory explicitly. It requires a Linux kernel with `/dev/fuse`
//! and either `CAP_SYS_ADMIN` for the privileged path or an executable,
//! setuid-capable `fusermount3`/`fusermount` helper for rootless mounting.
//! macOS uses the workspace's NFS transport instead.

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::path::PathBuf;
    use std::sync::Arc;

    let mut arguments = std::env::args_os();
    let _program = arguments.next();
    let Some(mountpoint) = arguments.next().map(PathBuf::from) else {
        eprintln!("usage: cargo run -p mount-rs-fuse --example native_mount -- PATH");
        eprintln!("PATH must be an existing directory and is mounted only on explicit invocation");
        return Ok(());
    };
    if arguments.next().is_some() {
        return Err("expected exactly one mountpoint path".into());
    }

    // The reference MemoryFs reports uid/gid zero. Leaving kernel permission
    // enforcement on would make a rootless demonstration fail before the
    // driver gets a request; applications should choose this deliberately.
    let options = mount_rs_fuse::mount::MountOptions {
        default_permissions: false,
        fsname: "mount-rs-example".to_owned(),
        ..Default::default()
    };
    let mounted = mount_rs_fuse::mount::mount(
        Arc::new(mount_rs_core::MemoryFs::empty()),
        &mountpoint,
        options,
    )
    .await?;
    eprintln!(
        "mounted {} with native Linux FUSE; press Ctrl-C to unmount",
        mounted.mountpoint().display()
    );
    tokio::signal::ctrl_c().await?;
    mounted.unmount().await?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("native Linux FUSE is unsupported on this platform; use the NFS transport");
}
