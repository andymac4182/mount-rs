//! Small process-level filesystem client used by scripts/demo-end-to-end.sh.
//!
//! This deliberately uses only std::fs. The mount-rs CLI owns the native
//! mount; this process proves that an independent Rust process can access the
//! mounted path with ordinary filesystem calls.

use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

fn main() {
    if let Err(error) = run() {
        eprintln!("rust-fs-io: {error}");
        std::process::exit(1);
    }
}

fn run() -> io::Result<()> {
    let mut arguments = env::args();
    let program = arguments.next().unwrap_or_else(|| "rust-fs-io".to_owned());
    let operation = arguments.next().ok_or_else(|| usage(&program))?;
    let root = PathBuf::from(arguments.next().ok_or_else(|| usage(&program))?);
    let name = arguments.next().ok_or_else(|| usage(&program))?;
    let expected = arguments.next().ok_or_else(|| usage(&program))?;

    if arguments.next().is_some() {
        return Err(usage(&program));
    }
    if operation != "write-read" && operation != "verify" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unknown operation '{operation}' (use write-read or verify)"),
        ));
    }
    if Path::new(&name)
        .file_name()
        .and_then(|value| value.to_str())
        != Some(name.as_str())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file name must not contain a directory separator",
        ));
    }

    let path = root.join(&name);
    let expected = expected.as_bytes();
    if operation == "write-read" {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&path)?;
        file.write_all(expected)?;
        file.sync_all()?;
        drop(file);
    }

    let actual = fs::read(&path)?;
    if actual != expected {
        return Err(io::Error::other(format!(
            "byte mismatch for {name}: expected {} bytes, read {} bytes",
            expected.len(),
            actual.len()
        )));
    }

    println!("rust-fs-io: {operation} ok ({name})");
    Ok(())
}

fn usage(program: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("usage: {program} <write-read|verify> <root> <file-name> <expected-bytes>"),
    )
}
