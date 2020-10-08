use super::OutputMethod;
use crate::{cargo, copy, elm};
use crate::Binaries;
use crate::Opt;
use ::futures::SinkExt;
use ::globset::{Glob, GlobSetBuilder};
use ::notify::{DebouncedEvent, RecommendedWatcher, RecursiveMode, Watcher};
use ::std::path::Path;
use ::std::process;
use ::std::thread;
use ::walkdir::WalkDir;
use tungstenite::Message;

fn event_path(event: &DebouncedEvent) -> Option<&Path> {
    match event {
        DebouncedEvent::NoticeRemove(x)
        | DebouncedEvent::Create(x)
        | DebouncedEvent::Write(x)
        | DebouncedEvent::Chmod(x)
        | DebouncedEvent::Remove(x)
        | DebouncedEvent::Rename(x, _) => Some(x),
        _ => None,
    }
}

// TODO: Make this use Tokio instead.
// It's a royal mess without it.
#[::tokio::main]
pub(crate) async fn start(opt: Opt, bins: Binaries) -> ::anyhow::Result<()> {
    copy(&opt.project_root)?;
    let output: Option<process::Output> = elm(
        &opt.project_root,
        &bins.elm,
        bins.terser.as_deref(),
        false,
        OutputMethod::Capture,
    )?;
    match output {
        Some(x) => {
            use ::std::io::Write;
            ::std::io::stdout().write(&x.stdout)?;
            ::std::io::stdout().write(&x.stderr)?;
            ::std::io::stdout().flush()?;
            ::anyhow::bail!("failed initial Elm build")
        }
        None => (),
    };

    let mut builder = GlobSetBuilder::new();
    builder.add(Glob::new("*.html")?);
    builder.add(Glob::new("*.css")?);
    builder.add(Glob::new("*.js")?);
    let copy_matcher = builder.build()?;
    let elm_matcher = Glob::new("*.elm")?.compile_matcher();
    let ignore_matcher = Glob::new("*#*")?.compile_matcher();
    macro_rules! is_copy {
        ($x:expr) => {
            (copy_matcher.is_match($x) && !ignore_matcher.is_match($x))
        };
    }
    macro_rules! is_elm {
        ($x:expr) => {
            (elm_matcher.is_match($x) && !ignore_matcher.is_match($x))
        };
    }
    let mopt = opt.clone();

    let mut listener = ::tokio::net::TcpListener::bind("127.0.0.1:9000")
        .await
        .expect("failed to bind tcp port");
    #[derive(Debug, Clone, PartialEq, Eq, ::serde::Serialize)]
    struct RefreshToken(u64);
    impl RefreshToken {
        fn new() -> Self {
            use ::rand::Rng;
            Self(::rand::thread_rng().gen())
        }
    }
    #[derive(Debug, Clone, ::serde::Serialize)]
    enum BrowserAction {
        // To prevent infinite reloading,
        // we generate a token to be associated with the
        // latest page refresh.
        // The client will use localStorage to keep
        // the last page refresh token they received,
        // and if they receive it again, ignore the refresh.
        // The only operation we need the RefreshToken to support
        // is equality comparison.
        RefreshPage(RefreshToken),
        DisplayError(String),
    }
    // If we start trying to do granular module reloading in the browser,
    // this will need to change to a broadcast channel of some sort.
    // For now, we assume that a page reload, which will bring the browser
    // to the correct state, is the only response other than showing the current error.
    let (tx, rx) = ::tokio::sync::watch::channel(None::<BrowserAction>);
    let (wtx, wrx) = ::std::sync::mpsc::channel();
    let mut watcher: RecommendedWatcher = Watcher::new(wtx, ::std::time::Duration::from_secs(0))?;
    watcher.watch(opt.project_root.join("src"), RecursiveMode::Recursive)?;
    thread::spawn(move || {
        enum ErrorState {
            Elm(process::Output),
            None,
        }
        let mut error_state = ErrorState::None;
        loop {
            match wrx.recv() {
                Ok(event) => {
                    println!("notify event: {:?}", event);
                    let path = match event_path(&event) {
                        Some(x) => x,
                        None => continue,
                    };
                    let mut refresh_copies = false;
                    let mut refresh_elm = false;
                    if path.is_dir() {
                        for entry in WalkDir::new(path) {
                            let entry = match entry {
                                Ok(x) => x,
                                Err(_) => continue,
                            };
                            if is_copy!(entry.path()) {
                                refresh_copies = true;
                            }
                            if is_elm!(entry.path()) {
                                refresh_elm = true;
                            }
                            if refresh_copies && refresh_elm {
                                break;
                            }
                        }
                    } else {
                        if is_copy!(path) {
                            refresh_copies = true;
                        }
                        if is_elm!(path) {
                            refresh_elm = true;
                        }
                    }
                    if refresh_copies {
                        let _ = copy(&mopt.project_root);
                    }
                    if refresh_elm {
                        let output: Option<process::Output> = elm(
                            &mopt.project_root,
                            &bins.elm,
                            bins.terser.as_deref(),
                            false,
                            OutputMethod::Capture,
                        )
                        .unwrap();
                        match output {
                            Some(x) => error_state = ErrorState::Elm(x),
                            None => error_state = ErrorState::None,
                        };
                    }
                    if refresh_copies || refresh_elm {
                        match error_state {
                            ErrorState::Elm(ref x) => tx
                                .broadcast(Some(BrowserAction::DisplayError(
                                    String::from_utf8_lossy(&x.stderr).into_owned(),
                                )))
                                .expect("channel closed"),
                            ErrorState::None => tx
                                .broadcast(Some(BrowserAction::RefreshPage(RefreshToken::new())))
                                .expect("channel closed"),
                        }
                    }
                }
                Err(e) => eprintln!("watch error: {}", e),
            }
        }
    });
    let project_root = opt.project_root.clone();
    thread::spawn(move || {
        cargo(&project_root, false, "run").unwrap();
    });
    // Give each connection its own task.
    while let Ok((stream, _)) = listener.accept().await {
        // Give each spawned task its own receiver.
        let mut rx = rx.clone();
        ::tokio::spawn(async move {
            ::foretry::async_try! { _, ::anyhow::Error | {
                let mut ws_stream = ::tokio_tungstenite::accept_async(stream).await?;
                // If the fs watcher has closed,
                // all connections should wind down.
                let mut count = 0;
                while let Some(action) = rx.recv().await {
                    match action {
                        Some(x) => {
                            println!("session refresh count: {}", count);
                            count += 1;
                            ws_stream.send(Message::Text(::serde_json::to_string(&x)?)).await?
                        },
                        None => continue,
                    }
                }
            } catch (e) {
                eprintln!("{}", e);
            }}
        });
    }
    Ok(())
}
