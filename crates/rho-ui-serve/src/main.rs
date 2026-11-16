//! The `rho-ui-serve` command. It relays the rho daemon's Unix socket and
//! runs `sh` scripts, so it runs on Unix only, as the machines rho-ui
//! reaches through anything but this one's own socket do.

#[cfg(unix)]
mod cli;

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    cli::main()
}

#[cfg(not(unix))]
#[expect(
    clippy::print_stderr,
    reason = "a command-line program says why it stops on stderr"
)]
fn main() -> std::process::ExitCode {
    eprintln!("rho-ui-serve runs on Unix only");
    std::process::ExitCode::FAILURE
}
