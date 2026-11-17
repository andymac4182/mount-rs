fn main() {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let runtime = tokio::runtime::Runtime::new().expect("create mount-rs Tokio runtime");
        let result = runtime.block_on(mount_rs_cli::run(std::env::args_os()));
        if let Err(error) = &result
            && error.abandons_running_work()
        {
            // The command gave up on work that may never finish, such as a
            // drive shutdown blocked on storage. Dropping the runtime would
            // wait for it after all, so leave without waiting.
            mount_rs_cli::runtime::exit_without_waiting(error);
        }
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
        report(&error);
        std::process::exit(error.exit_code());
    }
}

fn report(error: &mount_rs_cli::CliError) {
    eprintln!(
        "mount-rs: {}",
        mount_rs_cli::color::Color::from_env().red(error)
    );
}
