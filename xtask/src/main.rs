//! rho-ui's build tasks, run as `cargo xtask TASK`:
//!
//! - `web [--dev]`: the web build, in `target/web`: rho-ui built for
//!   `wasm32-unknown-unknown`, its JS glue, the page, and the fonts it
//!   loads. `rho-ui serve` built without one serves this.
//! - `dist`: a release `rho-ui` with the web build in it, gzipped, for
//!   `rho-ui serve` to serve from anywhere. On Windows, where there is no
//!   `rho-ui serve`, just the release `rho-ui`.
//! - `clippy-web`: clippy on the code the web build compiles, as CI runs it.
//! - `dev --rho CHECKOUT [-- ARGS]`: rho-ui against a rho checkout's
//!   daemon and rho-protocol, for a change that needs both; see [`dev`].
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
    process::{Command, ExitCode, Stdio},
};

use anyhow::{bail, ensure, Context as _, Result};
use sha2::{Digest as _, Sha256};

const USAGE: &str = "usage: cargo xtask TASK

  web [--dev]   build the web build into target/web (--dev: unoptimized,
                quicker to build); `cargo run -p rho-ui -- serve` serves it
  dist          build target/release/rho-ui with the web build in it
                (on Windows, which has no `rho-ui serve`, without)
  clippy-web    run clippy on the web build's code, as CI does
  dev --rho CHECKOUT [-- ARGS]
                run rho-ui with ARGS against the daemon and rho-protocol
                of the rho checkout CHECKOUT, in a rho home of its own";

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
        ["dev", "--rho", checkout] => dev(Path::new(checkout), &[]),
        ["dev", "--rho", checkout, "--", args @ ..] => dev(Path::new(checkout), args),
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
/// build on stable (see the module docs) and a C compiler for it, then
/// `args`.
fn wasm_cargo(subcommand: &str, args: &[&str]) -> Result<Command> {
    let mut command = cargo(&[subcommand, "--target", WASM_TARGET]);
    command.args(args).env("RUSTC_BOOTSTRAP", "wasm_thread");
    let unset: Vec<_> = [(WASM_CC, "clang"), (WASM_AR, "llvm-ar")]
        .into_iter()
        .filter(|(variable, _)| std::env::var_os(variable).is_none())
        .collect();
    if cfg!(target_os = "macos") && !unset.is_empty() {
        let llvm = homebrew_llvm()?;
        for (variable, tool) in unset {
            command.env(variable, llvm.join("bin").join(tool));
        }
    }
    Ok(command)
}

/// The variables the `cc` crate reads for the web build's C: its SQLite
/// (`sqlite-wasm-rs`).
const WASM_CC: &str = "CC_wasm32_unknown_unknown";
const WASM_AR: &str = "AR_wasm32_unknown_unknown";

/// Homebrew's LLVM, whose clang builds for wasm32, as Apple's can't.
fn homebrew_llvm() -> Result<PathBuf> {
    let missing = || {
        format!(
            "the web build's C needs a clang that builds for wasm32, which Apple's doesn't: \
             `brew install llvm`, or set {WASM_CC} and {WASM_AR} to another LLVM's \
             clang and llvm-ar"
        )
    };
    let prefix = Command::new("brew")
        .args(["--prefix", "llvm"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .with_context(missing)?;
    let llvm = PathBuf::from(String::from_utf8_lossy(&prefix.stdout).trim());
    ensure!(llvm.join("bin/clang").is_file(), "{}", missing());
    Ok(llvm)
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
    let mut build = wasm_cargo("build", &["-p", "rho-ui"])?;
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

/// Builds a release `rho-ui` with the web build in it, gzipped into
/// `target/web-gz`, which its `web` feature builds in. Without `rho-ui
/// serve`, on Windows, the release alone.
fn dist() -> Result<()> {
    // As `mod serve` in crates/rho-ui/src/main.rs.
    let serves = cfg!(unix);
    let mut build = cargo(&["build", "--release", "-p", "rho-ui"]);
    if serves {
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
    let serve = if serves {
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
    )?
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

/// Runs rho-ui against an unreleased rho: the daemon built from the rho
/// checkout `checkout`, and rho-protocol from its source, so a change that
/// spans both can be tried before rho releases it. `args` go to rho-ui.
///
/// Everything stays apart from your own rho and rho-ui:
///
/// - The daemon runs in the rho home `target/dev/rho-home`, which gets a
///   daemon of its own, as each rho home does, so yours and its sessions
///   carry on untouched. A dev daemon from an earlier run is stopped
///   first, so rho-ui starts the one just built. It keeps running after
///   rho-ui quits, until the next run.
/// - rho-ui remembers things in `target/dev/state`, so a dev build that
///   bumps the state database's schema doesn't empty yours (not on
///   Windows, where that folder follows `%APPDATA%`). Its settings are
///   yours.
/// - Cargo takes rho-protocol from the checkout through `--config`, and
///   `Cargo.lock`, which that rewrites, is put back after the build.
fn dev(checkout: &Path, args: &[&str]) -> Result<()> {
    let checkout = std::path::absolute(checkout)
        .with_context(|| format!("couldn't find the rho checkout {}", checkout.display()))?;
    let protocol = checkout.join("crates/rho-protocol");
    ensure!(
        protocol.join("Cargo.toml").is_file(),
        "{} isn't a rho checkout: it has no crates/rho-protocol",
        checkout.display()
    );

    run(cargo(&["build", "--bin", "rho"]).current_dir(&checkout))?;
    let rho = std::env::var_os("CARGO_TARGET_DIR")
        .map_or_else(|| checkout.join("target"), PathBuf::from)
        .join("debug")
        .join(format!("rho{}", std::env::consts::EXE_SUFFIX));

    let dev = root().join("target/dev");
    let home = dev.join("rho-home");
    seed(&home)?;
    let daemon = |verb: &str| {
        let mut command = Command::new(&rho);
        command.args(["daemon", verb]).env("RHO_HOME", &home);
        command
    };
    let running = daemon("status")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if running {
        run(&mut daemon("stop"))?;
    }

    let lock_file = root().join("Cargo.lock");
    let lock = fs::read(&lock_file).context("couldn't read Cargo.lock")?;
    let built = run(cargo(&["build", "-p", "rho-ui", "--config"]).arg(format!(
        "patch.crates-io.rho-protocol.path='{}'",
        protocol.display()
    )));
    if fs::read(&lock_file).ok().as_ref() != Some(&lock) {
        fs::write(&lock_file, &lock).context("couldn't put Cargo.lock back")?;
    }
    built?;

    if cfg!(windows) {
        eprintln!("xtask: rho-ui's state in %APPDATA% is your installed rho-ui's too");
    }
    run(Command::new(
        target_dir()
            .join("debug")
            .join(format!("rho-ui{}", std::env::consts::EXE_SUFFIX)),
    )
    .arg("--rho")
    .arg(&rho)
    .args(args)
    .env("RHO_HOME", &home)
    .env("XDG_STATE_HOME", dev.join("state")))
}

/// Gives the dev rho home `home` what it lacks of your rho home: a copy of
/// its settings, to change freely there, and a link to its credentials,
/// so a provider token that one daemon refreshes stays good for the other.
fn seed(home: &Path) -> Result<()> {
    let yours = your_rho_home()
        .context("couldn't find your rho home to seed the dev one: set RHO_HOME or HOME")?;
    fs::create_dir_all(home).with_context(|| format!("couldn't make {}", home.display()))?;
    let config = home.join("config.toml");
    if !config.exists() && yours.join("config.toml").is_file() {
        fs::copy(yours.join("config.toml"), &config).context("couldn't copy your rho settings")?;
    }
    let credentials = home.join("credentials");
    if credentials.symlink_metadata().is_err() && yours.join("credentials").is_dir() {
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(yours.join("credentials"), &credentials);
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_dir(yours.join("credentials"), &credentials);
        linked.context("couldn't link your rho credentials")?;
    }
    Ok(())
}

/// The rho home `rho` uses: `$RHO_HOME`, else `.rho` in `$HOME` (on
/// Windows, else in `%USERPROFILE%`).
fn your_rho_home() -> Option<PathBuf> {
    let set = |name| std::env::var_os(name).filter(|value| !value.is_empty());
    if let Some(home) = set("RHO_HOME") {
        return Some(PathBuf::from(home));
    }
    let home = set("HOME").or_else(|| set("USERPROFILE").filter(|_| cfg!(windows)))?;
    Some(PathBuf::from(home).join(".rho"))
}
