//! The command on Unix: reads its options, prints the URL with its token,
//! and serves until Ctrl-C.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    process::ExitCode,
    sync::Arc,
};

use rho_ui_serve::{router, Token};

/// The port it listens on unless told: any free one would do, but one that
/// stays put keeps a bookmark or a tunnel working across restarts.
const DEFAULT_PORT: u16 = 8765;

/// What the command line asks.
#[derive(Debug)]
struct Options {
    address: SocketAddr,
    /// The web build: `index.html`, and the JS and wasm it loads.
    dist: PathBuf,
}

const USAGE: &str = "usage: rho-ui-serve [--port PORT] [--bind ADDRESS] [--dist DIR]

Serves rho-ui in a browser at http://ADDRESS:PORT (127.0.0.1:8765 unless
told), with the web build in DIR (target/web, which scripts/build-web.sh
makes, unless told). Whoever reaches it can run commands as you: keep it on
127.0.0.1 and reach it from elsewhere through a tunnel such as `ssh -L`.";

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options {
        address: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_PORT),
        dist: PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/web")),
    };
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{arg} needs a value\n\n{USAGE}"))
        };
        match arg.as_str() {
            "--port" => {
                let port = value()?;
                let port = port
                    .parse()
                    .map_err(|_| format!("--port {port}: not a port"))?;
                options.address.set_port(port);
            }
            "--bind" => {
                let ip = value()?;
                let ip = ip
                    .parse()
                    .map_err(|_| format!("--bind {ip}: not an IP address"))?;
                options.address.set_ip(ip);
            }
            "--dist" => options.dist = PathBuf::from(value()?),
            "-h" | "--help" => return Err(USAGE.to_owned()),
            other => return Err(format!("unknown argument {other}\n\n{USAGE}")),
        }
    }
    Ok(options)
}

#[expect(
    clippy::print_stderr,
    reason = "a command-line program says what it does, and why it stops, on stderr"
)]
#[tokio::main]
pub(crate) async fn main() -> ExitCode {
    let options = match parse_args(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    if !options.dist.join("index.html").is_file() {
        eprintln!(
            "rho-ui-serve: no web build in {}: run scripts/build-web.sh, or name one with --dist",
            options.dist.display()
        );
        return ExitCode::FAILURE;
    }
    let token = Arc::new(Token::new());
    let listener = match tokio::net::TcpListener::bind(options.address).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("rho-ui-serve: can't listen on {}: {error}", options.address);
            return ExitCode::FAILURE;
        }
    };
    // The port the OS picked, for `--port 0`.
    let address = listener.local_addr().unwrap_or(options.address);
    eprintln!(
        "rho-ui-serve: open http://{address}/#token={}",
        token.as_str()
    );
    let served = axum::serve(listener, router(token, options.dist))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await;
    match served {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rho-ui-serve: {error}");
            ExitCode::FAILURE
        }
    }
}
