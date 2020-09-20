//! Doing build logic in shell scripts is lame.
//! Let's just use Rust.
use ::globset::{Glob, GlobSetBuilder};
use ::notify::{RecommendedWatcher, RecursiveMode, Watcher, DebouncedEvent};
use ::std::fs;
use ::std::net::TcpListener;
use ::std::path::{Path, PathBuf};
use ::std::process::{Command, Stdio};
use ::std::thread;
use ::structopt::StructOpt;
use ::walkdir::WalkDir;
use tungstenite::{server::accept, Message};

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

fn elm(
    project_root: &Path,
    elm: &Path,
    terser: Option<&Path>,
    release: bool,
) -> anyhow::Result<()> {
    let mut c = Command::new(elm);
    c.arg("make");
    if release {
        c.arg("--optimize");
    }
    c.arg(project_root.join("src/Main.elm"))
        .arg("--output")
        .arg(project_root.join("static/main.js"))
        .spawn()?
        .wait()
        .expect("failed to wait on child");
    if let Some(terser) = terser {
        sp! { terser ; project_root.join("static/main.js"), "--compress",
              "pure_funcs=\"F2,F3,F4,F5,F6,F7,F8,F9,A2,A3,A4,A5,A6,A7,A8,A9\",pure_getters,keep_fargs=false,unsafe_comps,unsafe"
              => terser ; "--mangle", "--output", project_root.join("static").join("main.js")
        }
    }

    Ok(())
}

/// Any source files we just need to copy into the output.
/// This currently means `.html` and `.css` files.
fn copy(project_root: &Path) -> anyhow::Result<()> {
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

fn event_path(event: &DebouncedEvent) -> Option<&Path> {
    match event {
        DebouncedEvent::NoticeWrite(x)
        | DebouncedEvent::NoticeRemove(x)
        | DebouncedEvent::Create(x)
        | DebouncedEvent::Write(x)
        | DebouncedEvent::Chmod(x)
        | DebouncedEvent::Remove(x)
        | DebouncedEvent::Rename(x, _) => Some(x),
        _ => None,
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
            )?;
            cargo(&opt.project_root, release, "build")?;
        }
        // Note that this does not handle recompiling the Rust parts
        // of the project. At least, not yet.
        Target::Dev => {
            copy(&opt.project_root)?;
            elm(&opt.project_root, &bins.elm, bins.terser.as_deref(), false)?;

            let mut builder = GlobSetBuilder::new();
            builder.add(Glob::new("*.html")?);
            builder.add(Glob::new("*.css")?);
            builder.add(Glob::new("*.js")?);
            let copy_matcher = builder.build()?;
            let elm_matcher = Glob::new("*.elm")?.compile_matcher();
            let ignore_matcher = Glob::new("*#*")?.compile_matcher();
            let mopt = opt.clone();
            thread::spawn(move || {
                let server = TcpListener::bind("127.0.0.1:9000").expect("failed to bind tcp port");
                let mut websocket = accept(server.accept().unwrap().0).unwrap();
                // Channel for file watcher to send us messages through
                let (tx, rx) = ::std::sync::mpsc::channel();

                let mut watcher: RecommendedWatcher =
                    Watcher::new(tx, ::std::time::Duration::from_secs(0)).unwrap();
                watcher.watch(&mopt.project_root.join("src"), RecursiveMode::Recursive).unwrap();

                loop {
                    match rx.recv() {
                        Ok(event) => {
                            println!("notify event: {:?}", event);
                            let path = match event_path(&event) {
                                Some(x) => x,
                                None => return,
                            };
                            let mut refresh_copies = false;
                            let mut refresh_elm = false;
                            if path.is_dir() {
                                for entry in WalkDir::new(path) {
                                    let entry = match entry {
                                        Ok(x) => x,
                                        Err(_) => continue,
                                    };
                                    if copy_matcher.is_match(entry.path()) && !ignore_matcher.is_match(entry.path()) {
                                        refresh_copies = true;
                                    }
                                    if elm_matcher.is_match(entry.path()) && !ignore_matcher.is_match(entry.path()) {
                                        refresh_elm = true;
                                    }
                                    if refresh_copies && refresh_elm {
                                        break;
                                    }
                                }
                            } else {
                                if copy_matcher.is_match(path) && !ignore_matcher.is_match(path) {
                                    refresh_copies = true;
                                }
                                if elm_matcher.is_match(path) && !ignore_matcher.is_match(path) {
                                    refresh_elm = true;
                                }
                            }
                            if refresh_copies {
                                let _ = copy(&mopt.project_root);
                            }
                            if refresh_elm {
                                elm(&mopt.project_root, &bins.elm, bins.terser.as_deref(), false).unwrap();
                            }
                            if refresh_copies || refresh_elm {
                                let _ = websocket.write_message(Message::text("reload"));
                                websocket = accept(server.accept().unwrap().0).unwrap();
                            }
                        },
                        Err(e) => eprintln!("watch error: {:?}", e),
                    }
                }
            });
            cargo(&opt.project_root, false, "run")?;
        }
        Target::Clean { html, elm, rust, doc } => {
            match (html, elm, rust, doc) {
                (true, _, _, _) => sp! { "rm" ; "-rf", opt.project_root.join("static") },
                (_, true, _, false) => sp! { "rm" ; "-rf", opt.project_root.join("elm-stuff") },
                // Once I figure out Elm documentation,
                // this should delete that.
                (_, true, _, true) => return Ok(()),
                (_, _, true, false) => sp! { "cargo" ; "clean",
                                              "--manifest-path", opt.project_root.join("Cargo.toml") },
                (_, _, true, true) => sp! { "cargo" ; "clean", "--doc",
                                             "--manifest-path", opt.project_root.join("Cargo.toml") },
                // This should delete *all* generated documentation.
                // That means once I figure out Elm documentation,
                // deleting it needs to be added here.
                (false, false, false, true) => sp! { "cargo" ; "clean", "--doc", "--manifest-path",
                                                      opt.project_root.join("Cargo.toml") },
                // This should delete *all* build artifacts.
                (false, false, false, false) => {
                    sp! { "rm" ; "-rf", opt.project_root.join("static") };
                    sp! { "rm" ; "-rf", opt.project_root.join("elm-stuff") };
                    sp! { "cargo" ; "clean", "--manifest-path", opt.project_root.join("Cargo.toml") };
                    return Ok(())
                }
            };
        }
    }
    Ok(())
}
