//! Scoped cleanup for a mount left behind by an earlier CLI process.
//!
//! This deliberately does not scan or unmount arbitrary filesystem entries.
//! The caller supplies one mountpoint, and only mount types produced by the
//! mount-rs transports are eligible for cleanup.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use crate::color::Color;

const STALE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
struct StaleCommand {
    program: &'static str,
    args: Vec<String>,
}

impl StaleCommand {
    fn display(&self) -> String {
        std::iter::once(self.program)
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

const fn host_is_macos() -> bool {
    cfg!(target_os = "macos")
}

fn stale_command(target: &Path, kind: &str, uid: u32, macos: bool) -> Option<StaleCommand> {
    if !is_cli_mount_type(kind) {
        return None;
    }

    let target = target.to_string_lossy().into_owned();
    if kind.starts_with("fuse") && uid != 0 {
        return Some(StaleCommand {
            program: "fusermount3",
            args: vec!["-u".into(), "-z".into(), target],
        });
    }

    let (program, flag) = if macos {
        ("umount", "-f")
    } else {
        ("umount", "-l")
    };
    Some(StaleCommand {
        program,
        args: vec![flag.into(), target],
    })
}

fn stale_needs_root(command: &StaleCommand, uid: u32, macos: bool) -> bool {
    uid != 0 && (command.program != "fusermount3" && !macos)
}

fn is_cli_mount_type(kind: &str) -> bool {
    kind.starts_with("fuse") || kind.starts_with("nfs") || kind == "9p"
}

/// Return the last mount-table type for the exact target.
#[cfg(target_os = "linux")]
fn mount_type_from_linux_table(table: &str, target: &Path) -> Option<String> {
    let target = target.to_string_lossy();
    table
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let _source = fields.next()?;
            let path = fields.next()?;
            let kind = fields.next()?;
            Some((unescape_mount_path(path), kind))
        })
        .filter(|(path, _kind)| path == target.as_ref())
        .map(|(_path, kind)| kind.to_owned())
        .next_back()
}

#[cfg(target_os = "linux")]
fn unescape_mount_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut result = String::with_capacity(path.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 3 < bytes.len() {
            let octal = &bytes[index + 1..index + 4];
            if let Some(value) = octal_value(octal) {
                result.push(char::from(value));
                index += 4;
                continue;
            }
        }
        let character = path[index..]
            .chars()
            .next()
            .expect("a byte index must point at a character boundary");
        result.push(character);
        index += character.len_utf8();
    }
    result
}

#[cfg(target_os = "linux")]
fn octal_value(octal: &[u8]) -> Option<u8> {
    octal.iter().try_fold(0_u8, |value, digit| {
        if !(b'0'..=b'7').contains(digit) {
            return None;
        }
        value.checked_mul(8)?.checked_add(*digit - b'0')
    })
}

async fn stale_type(target: &Path) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        mount_rs_nfs::mount_entry_at(target, mount_rs_nfs::NfsPlatform::Macos)
            .await
            .ok()
            .flatten()
            .map(|entry| entry.fs_type)
    }

    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/self/mounts")
            .ok()
            .and_then(|table| mount_type_from_linux_table(&table, target))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = target;
        None
    }
}

/// Detach a stale mount at exactly `target`, if it is a mount this CLI could
/// have created. Failure is reported but is intentionally not fatal: the
/// subsequent native mount operation owns the final error.
pub(crate) async fn unmount_stale(target: &Path, uid: u32, color: Color) {
    let Some(kind) = stale_type(target).await else {
        return;
    };
    let macos = host_is_macos();
    let Some(command) = stale_command(target, &kind, uid, macos) else {
        return;
    };

    if stale_needs_root(&command, uid, macos) {
        eprintln!(
            "{} {} is still mounted ({})\n  clear it with {}",
            color.yellow("!"),
            color.cyan(target.display()),
            kind,
            color.bold(format!("sudo {}", command.display()))
        );
        return;
    }

    println!(
        "{} {} {} ({})",
        color.yellow("!"),
        color.bold("unmounting stale"),
        color.cyan(target.display()),
        kind
    );

    let result = tokio::process::Command::new(command.program)
        .args(&command.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    let output = match tokio::time::timeout(STALE_TIMEOUT, result).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            eprintln!(
                "{} could not run {}: {}",
                color.red("error:"),
                command.program,
                error
            );
            return;
        }
        Err(_) => {
            eprintln!(
                "{} could not unmount {}: no answer within {}ms",
                color.red("error:"),
                target.display(),
                STALE_TIMEOUT.as_millis()
            );
            #[cfg(target_os = "macos")]
            eprintln!("{}", color.dim(mount_rs_nfs::consent_advice(target)));
            return;
        }
    };

    if output.status.success() {
        return;
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = if stderr.trim().is_empty() {
        format!("{} exited {:?}", command.program, output.status.code())
    } else {
        stderr.trim().to_owned()
    };
    eprintln!(
        "{} could not unmount {}: {}",
        color.red("error:"),
        target.display(),
        detail
    );
}

pub(crate) fn stale_command_line(target: &Path, transport: &str, uid: u32) -> Option<String> {
    stale_command(target, transport, uid, host_is_macos()).map(|command| command.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "linux")]
    fn malformed_mount_escapes_remain_literal_without_overflow() {
        for path in [r"/tmp/\999", r"/tmp/\777", r"/tmp/\!00", r"/tmp/\ab"] {
            assert_eq!(unescape_mount_path(path), path);
        }
        assert_eq!(
            unescape_mount_path(r"/tmp/with\040space"),
            "/tmp/with space"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn mount_table_uses_last_exact_target_and_unescapes_linux_paths() {
        let target = Path::new("/tmp/mount point");
        let table = concat!(
            "none /tmp/mount\\040point ext4 rw 0 0\n",
            "none /tmp/mount\\040point fuse.mountx rw 0 0\n",
            "none /tmp/mount\\040point-child nfs rw 0 0\n",
        );
        assert_eq!(
            mount_type_from_linux_table(table, target).as_deref(),
            Some("fuse.mountx")
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn mount_table_decodes_the_four_proc_escapes() {
        let target = Path::new("/tmp/a b\tc\nd\\e");
        let table = "none /tmp/a\\040b\\011c\\012d\\134e 9p rw 0 0\n";
        assert_eq!(
            mount_type_from_linux_table(table, target).as_deref(),
            Some("9p")
        );
    }

    #[test]
    fn cleanup_is_limited_to_mounts_owned_by_this_cli() {
        assert!(is_cli_mount_type("fuse.mountx"));
        assert!(is_cli_mount_type("fuseblk"));
        assert!(is_cli_mount_type("nfs4"));
        assert!(is_cli_mount_type("9p"));
        assert!(!is_cli_mount_type("9p2"));
        assert!(!is_cli_mount_type("ext4"));
    }

    #[test]
    fn stale_command_matches_upstream_privilege_and_platform_rules() {
        let target = Path::new("/tmp/mount-rs");
        assert_eq!(
            stale_command(target, "fuse.mountx", 1000, false),
            Some(StaleCommand {
                program: "fusermount3",
                args: vec!["-u".into(), "-z".into(), "/tmp/mount-rs".into()],
            })
        );
        assert_eq!(
            stale_command(target, "nfs4", 1000, true),
            Some(StaleCommand {
                program: "umount",
                args: vec!["-f".into(), "/tmp/mount-rs".into()],
            })
        );
        let linux_nfs = stale_command(target, "nfs", 1000, false).unwrap();
        assert!(stale_needs_root(&linux_nfs, 1000, false));
        let macos_nfs = stale_command(target, "nfs", 1000, true).unwrap();
        assert!(!stale_needs_root(&macos_nfs, 1000, true));
    }
}
