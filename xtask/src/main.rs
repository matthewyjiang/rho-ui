//! rho-ui's build tasks, run as `cargo xtask TASK`:
//!
//! - `web [--dev]`: the web build, in `target/web`: rho-ui built for
//!   `wasm32-unknown-unknown`, its JS glue, the page, and the fonts it
//!   loads. `rho-ui serve` built without one serves this.
//! - `dist`: a release `rho-ui` with the web build in it, gzipped, for
//!   `rho-ui serve` to serve from anywhere. On Windows, where there is no
//!   `rho-ui serve`, just the release `rho-ui`.
//! - `clippy-web`: clippy on the code the web build compiles, as CI runs it.
//!
//! The wasm build needs one thing stable Rust doesn't give:
//! `wasm_thread`, under GPUI's web platform, names a nightly feature even
//! when unused, so these let that one crate build on stable with
//! `RUSTC_BOOTSTRAP=wasm_thread`.
#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a build tool says what it built, and why it failed, on the terminal"
)]

use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

use anyhow::{bail, ensure, Context as _, Result};
use sha2::{Digest as _, Sha256};

const USAGE: &str = "usage: cargo xtask TASK

  web [--dev]   build the web build into target/web (--dev: unoptimized,
                quicker to build); `cargo run -p rho-ui -- serve` serves it
  dist          build target/release/rho-ui with the web build in it
                (on Windows, which has no `rho-ui serve`, without)
  clippy-web    run clippy on the web build's code, as CI does";

const WASM_TARGET: &str = "wasm32-unknown-unknown";

/// Noto Sans (notofonts v2.015) and `JetBrainsMono` Nerd Font (Nerd Fonts
/// v3.5.1, with the icons shell prompts draw), both OFL: the default UI
/// and code fonts (`settings.rs`), in the styles rho-ui draws, as the page
/// loads them (`web.rs`). A browser has no fonts GPUI can read.
const FONTS: [(&str, &str); 8] = [
    (NOTO, "f3961a9cde016d41a4879aecda1474d3a36d6bf54fa0e4643de029cc2248b0e8 NotoSans-Regular.ttf"),
    (NOTO, "87cb2d84472a7d66da659ee47b6cdb9552326e8c128245231f191b6ac72529d9 NotoSans-Bold.ttf"),
    (NOTO, "678288f868807d4d64a6f3b51466871d117d915780381ce9d0ed4b3bcbd06d37 NotoSans-Italic.ttf"),
    (NOTO, "3d367743f371f28671d2764e911a53d7c20ec9b6aa8791d059e7090389fc52a5 NotoSans-BoldItalic.ttf"),
    (MONO, "1c680e8cde9fcf8b88a5605ce8d1fb94dd3fb15841f7ca7bf4c55664855e5611 JetBrainsMonoNerdFont-Regular.ttf"),
    (MONO, "e490660ad75e0b152c93b1604c2ea1a4ea675f1b8a3fe0d005b1998568f99f1f JetBrainsMonoNerdFont-Bold.ttf"),
    (MONO, "71d5466cc3ab31bee38b4e2c5cc99c2b41a749c4973ed4579b814ec992b0738e JetBrainsMonoNerdFont-Italic.ttf"),
    (MONO, "19a2e3af8ccf99954941ea07e0a29df27574500f12016d86d6c4c059b825ffe1 JetBrainsMonoNerdFont-BoldItalic.ttf"),
];
const NOTO: &str = "https://raw.githubusercontent.com/notofonts/notofonts.github.io/28b15b4b43b7bed62b5cf6e6b0b5ff5846270535/fonts/NotoSans/unhinted/ttf";
const MONO: &str = "https://raw.githubusercontent.com/ryanoasis/nerd-fonts/v3.5.1/patched-fonts/JetBrainsMono/Ligatures";

/// How the wasm is built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Profile {
    Release,
    Dev,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let ran = match args.as_slice() {
        ["web"] => web(Profile::Release),
        ["web", "--dev"] => web(Profile::Dev),
        ["dist"] => dist(),
        ["clippy-web"] => clippy_web(),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match ran {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// The workspace's root.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask sits in the workspace")
        .to_owned()
}

/// Where cargo builds: `CARGO_TARGET_DIR`, else the workspace's `target`.
fn target_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root().join("target"), PathBuf::from)
}

/// `cargo` with `args`, from the workspace's root.
fn cargo(args: &[&str]) -> Command {
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command.args(args).current_dir(root());
    command
}

/// `cargo SUBCOMMAND` for the web build's target, with what that needs to
/// build on stable (see the module docs), then `args`.
fn wasm_cargo(subcommand: &str, args: &[&str]) -> Command {
    let mut command = cargo(&[subcommand, "--target", WASM_TARGET]);
    command.args(args).env("RUSTC_BOOTSTRAP", "wasm_thread");
    command
}

fn run(command: &mut Command) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("couldn't run {command:?}"))?;
    ensure!(status.success(), "{command:?} failed: {status}");
    Ok(())
}

/// Builds the web build into `target/web`.
fn web(profile: Profile) -> Result<()> {
    let mut build = wasm_cargo("build", &["-p", "rho-ui"]);
    match profile {
        Profile::Release => {
            build.arg("--release");
        }
        // Ghostty's Zig, built for Debug, logs through a JS import the
        // page doesn't have; its other modes log nothing. ReleaseSafe
        // keeps its checks.
        Profile::Dev => {
            build.env("LIBGHOSTTY_VT_SYS_OPTIMIZE", "ReleaseSafe");
        }
    }
    run(&mut build)?;

    let out = root().join("target/web");
    if out.exists() {
        fs::remove_dir_all(&out).with_context(|| format!("couldn't clear {}", out.display()))?;
    }
    let profile_dir = match profile {
        Profile::Release => "release",
        Profile::Dev => "debug",
    };
    let wasm = target_dir()
        .join(WASM_TARGET)
        .join(profile_dir)
        .join("rho-ui.wasm");
    wasm_bindgen_cli_support::Bindgen::new()
        .input_path(&wasm)
        .out_name("rho-ui")
        .web(true)?
        .typescript(false)
        // `init()` with no argument fetches `rho-ui_bg.wasm` beside the JS,
        // as the CLI's glue does; the library leaves that out unless told.
        .omit_default_module_path(false)
        // Function names, a quarter of a release's wasm, only name a stack
        // trace's frames.
        .remove_name_section(profile == Profile::Release)
        .generate(&out)
        .with_context(|| format!("wasm-bindgen couldn't make glue for {}", wasm.display()))?;
    fs::copy(
        root().join("crates/rho-ui/web/index.html"),
        out.join("index.html"),
    )
    .context("couldn't copy the page")?;
    fonts(&out.join("fonts"))?;
    println!(
        "built {}; serve it with `cargo run -p rho-ui -- serve`",
        out.display()
    );
    Ok(())
}

/// Puts [`FONTS`] in `dir`, each downloaded once into `target/web-fonts`
/// and checked against its hash.
fn fonts(dir: &Path) -> Result<()> {
    let cache = root().join("target/web-fonts");
    fs::create_dir_all(&cache)?;
    fs::create_dir_all(dir)?;
    for (base, pinned) in FONTS {
        let (sum, name) = pinned.split_once(' ').expect("each font is `HASH NAME`");
        let file = cache.join(name);
        if !file.is_file() {
            run(Command::new("curl")
                .args(["-sSLf", "-o"])
                .arg(&file)
                .arg(format!("{base}/{name}")))?;
        }
        let bytes = fs::read(&file)?;
        let got = format!("{:x}", Sha256::digest(&bytes));
        if got != sum {
            fs::remove_file(&file)?;
            bail!("{name} has SHA-256 {got}, not the pinned {sum}");
        }
        fs::write(dir.join(name), bytes)?;
    }
    Ok(())
}

/// Builds a release `rho-ui`, with the web build in it where `rho-ui serve`
/// runs, on Unix: the web build, gzipped into `target/web-gz`, which its
/// `web` feature builds in. Elsewhere it would be dead weight.
fn dist() -> Result<()> {
    let mut build = cargo(&["build", "--release", "-p", "rho-ui"]);
    if cfg!(unix) {
        web(Profile::Release)?;
        let web = root().join("target/web");
        let gz = root().join("target/web-gz");
        if gz.exists() {
            fs::remove_dir_all(&gz)?;
        }
        gzip_tree(&web, &gz)?;
        build.args(["--features", "web"]);
    }
    run(&mut build)?;
    let binary = target_dir()
        .join("release")
        .join(format!("rho-ui{}", std::env::consts::EXE_SUFFIX));
    let serve = if cfg!(unix) {
        "; run `rho-ui serve` for the browser"
    } else {
        ""
    };
    println!("built {}{serve}", binary.display());
    Ok(())
}

/// Copies every file under `from` to the same place under `to`, gzipped.
fn gzip_tree(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        if entry.file_type()?.is_dir() {
            gzip_tree(&source, &target)?;
        } else {
            let mut gz = flate2::write::GzEncoder::new(
                fs::File::create(&target)?,
                flate2::Compression::best(),
            );
            gz.write_all(&fs::read(&source)?)?;
            gz.finish()?;
        }
    }
    Ok(())
}

/// Clippy on what the web build compiles, warnings as errors.
fn clippy_web() -> Result<()> {
    let config = clippy_web_config()?;
    run(wasm_cargo(
        "clippy",
        &[
            "--locked",
            "-p",
            "rho-ui-core",
            "-p",
            "rho-ui",
            "--",
            "-D",
            "warnings",
        ],
    )
    .env("CLIPPY_CONF_DIR", config))
}

/// std's clock calls, which panic in a browser: `web_time` has the ones
/// that don't. They are only banned for the web build, as `web_time` is
/// std's own elsewhere, which clippy would flag too.
const BROWSER_PANICS: &str = r#"
    { path = "std::time::Instant::now", reason = "panics in a browser: use web_time::Instant" },
    { path = "std::time::SystemTime::now", reason = "panics in a browser: use web_time::SystemTime" },"#;

/// A folder holding the workspace's `clippy.toml` with [`BROWSER_PANICS`]
/// added to its `disallowed-methods`, for clippy on the web build.
fn clippy_web_config() -> Result<PathBuf> {
    let base =
        fs::read_to_string(root().join("clippy.toml")).context("couldn't read clippy.toml")?;
    let list = "disallowed-methods = [";
    ensure!(
        base.contains(list),
        "clippy.toml has no `{list}` for xtask to add the browser's bans to"
    );
    let dir = target_dir().join("clippy-web");
    fs::create_dir_all(&dir).with_context(|| format!("couldn't make {}", dir.display()))?;
    fs::write(
        dir.join("clippy.toml"),
        base.replacen(list, &format!("{list}{BROWSER_PANICS}"), 1),
    )
    .context("couldn't write the web build's clippy.toml")?;
    Ok(dir)
}
