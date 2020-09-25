//! Doing build logic in shell scripts is lame.
//! Let's just use Rust.
use ::globset::{Glob, GlobSetBuilder};
use ::std::fs;
use ::std::path::{Path, PathBuf};
use ::std::process::{self, Command, Stdio};
use ::structopt::StructOpt;
use ::walkdir::WalkDir;

mod server;

#[derive(Debug, StructOpt, Clone)]
struct Opt {
    project_root: PathBuf,
    #[structopt(subcommand)]
    target: Target,
}

#[derive(Debug, StructOpt, Clone)]
enum Target {
    /// Builds and runs the whole app
    Run {
        /// Build artifacts in release mode, with optimizations
        #[structopt(long, short)]
        release: bool,
    },
    /// Builds the whole app
    Build {
        /// Build artifacts in release mode, with optimizations
        #[structopt(long, short)]
        release: bool,
    },
    /// Starts the app in full on live reloading dev mode
    Dev,
    /// Remove build artifacts
    Clean {
        /// Whether to narrow cleaning to the site directory
        #[structopt(long)]
        html: bool,
        /// Whether to narrow cleaning to the Elm build artifacts
        #[structopt(long)]
        elm: bool,
        /// Whether to narrow cleaning to the Rust build artifacts
        #[structopt(long)]
        rust: bool,
        /// Whether to narrow cleaning to documentation
        #[structopt(long)]
        doc: bool,
    },
}

/// A little abstraction because I messed this up before, lol.
struct NormalizedClean {
    // These are normalized.
    // If true, do it.
    html: bool,
    elm: bool,
    rust: bool,
    // This one is special,
    // in that it alters the others.
    doc: bool,
}
impl NormalizedClean {
    /// Normalize from narrowing form.
    fn new(html: bool, elm: bool, rust: bool, doc: bool) -> Self {
        match (html, elm, rust) {
            (false, false, false) => Self {
                html: true,
                elm: true,
                rust: true,
                doc,
            },
            (html, elm, rust) => Self {
                html,
                elm,
                rust,
                doc,
            },
        }
    }
}

fn cargo(project_root: &Path, release: bool, subcommand: &str) -> anyhow::Result<()> {
    let mut c = Command::new("cargo");
    c.arg(subcommand);
    if release {
        c.arg("--release");
    }
    c.arg("--manifest-path")
        .arg(project_root.join("Cargo.toml"))
        .spawn()?
        .wait()
        .expect("failed to wait on child");

    Ok(())
}

/// Run some commands and pipe between them
macro_rules! sp {
    // No pipe case
    ($cmd:expr ; $($arg:expr),*) => {
        Command::new($cmd) $(.arg($arg))* .spawn()?.wait().expect("failed to wait on child");
    };
    // Match front of invocation
    ($cmd:expr ; $($arg:expr),* => $($t:tt)*) => {
        let mut c = Command::new($cmd) $(.arg($arg))* .stdout(Stdio::piped()) .spawn()?;
        if let Some(c_out) = c.stdout.take() {
            // Next command
            sp! { (c_out) -> $($t)* }
        }
    };
    // Inner invocation
    (($pre:expr) -> $cmd:expr ; $($arg:expr),* => $($t:tt)* ) => {
        let mut c = Command::new($cmd) $(.arg($arg))* .stdin($pre) .stdout(Stdio::piped()) .spawn()?;
        if let Some(c_out) = c.stdout.take() {
            // Next command
            sp! { (c_out) -> $($t)* }
        }
    };
    // Final invocation
    (($pre:expr) -> $cmd:expr ; $($arg:expr),*) => {
        Command::new($cmd) $(.arg($arg))* .stdin($pre).spawn()?.wait().expect("failed to wait on child");
    };
}

pub(crate) enum OutputMethod {
    Forward,
    Capture,
}

fn elm(
    project_root: &Path,
    elm: &Path,
    terser: Option<&Path>,
    release: bool,
    out: OutputMethod,
) -> anyhow::Result<Option<process::Output>> {
    let mut c = Command::new(elm);
    c.arg("make");
    if release {
        c.arg("--optimize");
    }
    c.arg(project_root.join("src/Main.elm"))
        .arg("--output")
        .arg(project_root.join("static/main.js"));
    match out {
        OutputMethod::Forward => {
            c.spawn()?.wait().expect("failed to wait on child");
        }
        OutputMethod::Capture => {
            let output = c.output().expect("failed to wait on child");
            if !output.status.success() {
                return Ok(Some(output));
            }
        }
    }
    // We don't collect the output for this,
    // since its success or failure is entirely
    // contingent on the success or failure of `elm make`.
    if let Some(terser) = terser {
        sp! { terser ; project_root.join("static/main.js"), "--compress",
              "pure_funcs=\"F2,F3,F4,F5,F6,F7,F8,F9,A2,A3,A4,A5,A6,A7,A8,A9\",pure_getters,keep_fargs=false,unsafe_comps,unsafe"
              => terser ; "--mangle", "--output", project_root.join("static").join("main.js")
        }
    }

    Ok(None)
}

/// Any source files we just need to copy into the output.
/// This currently means `.html` and `.css` files.
pub(crate) fn copy(project_root: &Path) -> anyhow::Result<()> {
    let mut builder = GlobSetBuilder::new();
    builder.add(Glob::new("*.html")?);
    builder.add(Glob::new("*.css")?);
    builder.add(Glob::new("*.js")?);
    let matcher = builder.build()?;
    let ignore_matcher = Glob::new("*#*")?.compile_matcher();
    for entry in WalkDir::new(project_root.join("src")) {
        let entry = entry?;
        if matcher.is_match(entry.path()) && !ignore_matcher.is_match(entry.path()) {
            let depth = entry.depth();
            let rel = entry
                .path()
                .components()
                .rev()
                .take(depth)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<PathBuf>();
            let dest = project_root.join("static").join(&rel);
            println!("src: {:?}, dest: {:?}", rel, dest);
            ::fsio::file::ensure_exists(&dest).map_err(|e| ::anyhow::anyhow!(e))?;
            fs::copy(entry.path(), &dest)?;
        }
    }

    Ok(())
}

/// A collection of the binaries we need to
/// build the whole app.
/// Useful binaries we can go without
/// shall be `Option<PathBuf>`.
struct Binaries {
    /// The [Elm](https://elm-lang.org) compiler!
    elm: PathBuf,
    terser: Option<PathBuf>,
}
impl Binaries {
    /// Find, build, or otherwise obtain the binaries we
    /// need to build the whole app.
    /// This does not include `rustc` or `cargo`.
    fn collect() -> Result<Binaries, anyhow::Error> {
        use ::which::which;
        let bins = Self {
            elm: match which("elm") {
                Ok(x) => x,
                Err(_) => ::anyhow::bail!("we don't handle not having Elm yet"),
            },
            terser: match which("terser") {
                Ok(x) => Some(x),
                // We don't handle not having Terser yet, but it's not fatal.
                // TODO: Fetch Terser.
                Err(_) => None,
            },
        };
        Ok(bins)
    }
}

fn main() -> ::anyhow::Result<()> {
    let opt = Opt::from_args();
    // println!("Args: {:?}", opt);
    let bins = Binaries::collect()?;
    match opt.target {
        Target::Run { release } => {
            copy(&opt.project_root)?;
            elm(
                &opt.project_root,
                &bins.elm,
                bins.terser.as_deref(),
                release,
                OutputMethod::Forward,
            )?;
            cargo(&opt.project_root, release, "run")?;
        }
        Target::Build { release } => {
            copy(&opt.project_root)?;
            elm(
                &opt.project_root,
                &bins.elm,
                bins.terser.as_deref(),
                release,
                OutputMethod::Forward,
            )?;
            cargo(&opt.project_root, release, "build")?;
        }
        // Note that this does not handle recompiling the Rust parts
        // of the project. At least, not yet.
        Target::Dev => {
            server::start(opt, bins)?;
        }
        Target::Clean {
            html,
            elm,
            rust,
            doc,
        } => {
            let normal = NormalizedClean::new(html, elm, rust, doc);
            if normal.html {
                if normal.doc {
                    // There are no docs for this target, yet.
                } else {
                    sp! { "rm" ; "-rf", opt.project_root.join("static") };
                }
            }
            if normal.elm {
                if normal.doc {
                    // There are no docs for this target, yet.
                } else {
                    sp! { "rm" ; "-rf", opt.project_root.join("elm-stuff") };
                }
            }
            if normal.rust {
                if normal.doc {
                    sp! { "cargo" ; "clean", "--doc",
                    "--manifest-path", opt.project_root.join("Cargo.toml") };
                } else {
                    sp! { "cargo" ; "clean", "--manifest-path", opt.project_root.join("Cargo.toml") };
                }
            }
        }
    }
    Ok(())
}
