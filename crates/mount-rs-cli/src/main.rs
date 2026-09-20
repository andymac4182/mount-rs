#[tokio::main]
async fn main() {
    if let Err(error) = mount_rs_cli::run(std::env::args_os()).await {
        eprintln!(
            "mount-rs: {}",
            mount_rs_cli::color::Color::from_env().red(&error)
        );
        std::process::exit(error.exit_code());
    }
}
