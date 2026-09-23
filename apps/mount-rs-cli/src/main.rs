fn main() {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let runtime = tokio::runtime::Runtime::new().expect("create mount-rs Tokio runtime");
        let result = runtime.block_on(mount_rs_cli::run(std::env::args_os()));
        // Detached transport tasks must finish and release their provider
        // handles before stopping FoundationDB's process-wide client network.
        drop(runtime);
        result
    }));

    #[cfg(all(
        feature = "foundationdb",
        any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "aarch64"),
            all(target_os = "macos", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64"),
        )
    ))]
    if let Err(error) = mount_rs_foundationdb::shutdown_client_network() {
        eprintln!("mount-rs: could not stop FoundationDB client network: {error}");
        // The C API forbids normal process exit while its network thread runs.
        std::process::abort();
    }

    let result = match outcome {
        Ok(result) => result,
        Err(payload) => std::panic::resume_unwind(payload),
    };

    if let Err(error) = result {
        eprintln!(
            "mount-rs: {}",
            mount_rs_cli::color::Color::from_env().red(&error)
        );
        std::process::exit(error.exit_code());
    }
}
