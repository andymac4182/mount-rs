//! Opt-in native lifecycle acceptance.
//!
//! The ordinary CLI suite is deliberately mount-free. This ignored test is a
//! separate Linux-only check for a real FUSE kernel mount and Ctrl-C cleanup.

#![cfg(target_os = "linux")]

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[test]
#[ignore = "requires an opt-in Linux FUSE setup; see the test command in the CLI README"]
fn cli_fuse_subprocess_mounts_and_unmounts_on_sigint() {
    if std::env::var("MOUNT_RS_CLI_NATIVE_FUSE").ok().as_deref() != Some("1") {
        eprintln!("set MOUNT_RS_CLI_NATIVE_FUSE=1 to opt into native FUSE acceptance");
        return;
    }

    let mountpoint = unique_mountpoint();
    fs::create_dir(&mountpoint).expect("create disposable native mountpoint");
    let mut child = Command::new(env!("CARGO_BIN_EXE_mount-rs"))
        .args([
            "mount",
            "--transport",
            "fuse",
            "--empty",
            "--quiet",
            mountpoint.to_str().expect("UTF-8 temporary mountpoint"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mount-rs FUSE subprocess");

    let (line_sender, line_receiver) = mpsc::channel::<String>();
    let stdout = child.stdout.take().expect("capture CLI stdout");
    let stdout_thread = thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = line_sender.send(line);
        }
    });
    let stderr = child.stderr.take().expect("capture CLI stderr");
    let stderr_thread = thread::spawn(move || {
        BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
            .collect::<Vec<_>>()
    });

    let mut output = Vec::new();
    let ready = wait_for_mount(&mut child, &line_receiver, &mut output);
    assert!(ready, "FUSE subprocess did not mount; output: {output:?}");
    assert!(
        is_mounted_at(&mountpoint),
        "kernel did not report the mountpoint"
    );

    // This is the lifecycle path under test: SIGINT is handled by the CLI,
    // which calls the transport's real unmount operation before exiting.
    // SAFETY: the child is the live subprocess just spawned above, and no
    // other process identifier is used.
    let signal_result = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) };
    assert_eq!(signal_result, 0, "send SIGINT to mount-rs");
    let status = wait_for_exit(&mut child);
    assert!(status.success(), "CLI did not exit cleanly: {status}");

    stdout_thread.join().expect("join CLI stdout reader");
    let stderr_lines = stderr_thread.join().expect("join CLI stderr reader");
    output.extend(line_receiver.try_iter());
    assert!(
        output.iter().any(|line| line.contains("unmounted")),
        "CLI did not report unmount: {output:?}"
    );
    assert!(
        !is_mounted_at(&mountpoint),
        "mountpoint remained mounted after CLI exit; stdout={output:?}, stderr={stderr_lines:?}"
    );
    fs::remove_dir(&mountpoint).expect("remove disposable native mountpoint");
}

fn unique_mountpoint() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "mount-rs-cli-native-{}-{nanos}",
        std::process::id()
    ))
}

fn wait_for_mount(
    child: &mut std::process::Child,
    receiver: &mpsc::Receiver<String>,
    output: &mut Vec<String>,
) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                let mounted = strip_ansi(&line).contains("mounted fuse");
                output.push(line);
                if mounted {
                    return true;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if child.try_wait().expect("poll FUSE subprocess").is_some() {
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    false
}

fn wait_for_exit(child: &mut std::process::Child) -> std::process::ExitStatus {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().expect("poll CLI exit") {
            return status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "CLI did not exit after SIGINT"
        );
        thread::sleep(Duration::from_millis(100));
    }
}

fn strip_ansi(value: &str) -> String {
    let mut plain = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character != '\u{1b}' {
            plain.push(character);
            continue;
        }
        if chars.next() != Some('[') {
            continue;
        }
        for final_byte in chars.by_ref() {
            if final_byte.is_ascii_alphabetic() {
                break;
            }
        }
    }
    plain
}

fn is_mounted_at(target: &std::path::Path) -> bool {
    let target = target.to_string_lossy();
    fs::read_to_string("/proc/self/mounts")
        .map(|table| {
            table.lines().any(|line| {
                let mut fields = line.split_whitespace();
                let _source = fields.next();
                fields.next().is_some_and(|path| path == target)
            })
        })
        .unwrap_or(false)
}
