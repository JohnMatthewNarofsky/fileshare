use super::OutputMethod;
use crate::Binaries;
use crate::Opt;
use crate::{cargo, copy, elm};
use ::futures::SinkExt;
use ::globset::{Glob, GlobSetBuilder};
use ::notify::{DebouncedEvent, RecommendedWatcher, RecursiveMode, Watcher};
use ::serde::Deserialize;
use ::std::path::Path;
use ::std::process;
use ::std::thread;
use ::walkdir::WalkDir;
use tungstenite::Message;
use ::tokio_rustls::rustls;

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

// This TLS stuff... Is probably going to be used in the other part, too.
// When I'm setting up large uploads via WebSockets.
// I'll probably throw it into another crate in the workspace.
#[derive(Deserialize, Debug)]
struct RocketConfig {
    global: RocketConfigGlobal,
}
#[derive(Deserialize, Debug)]
struct RocketConfigGlobal {
    tls: RocketConfigTls,
}
#[derive(Deserialize, Debug)]
struct RocketConfigTls {
    certs: ::std::path::PathBuf,
    key: ::std::path::PathBuf,
}

fn load_certs(filename: &Path) -> Vec<rustls::Certificate> {
    let certfile = ::std::fs::File::open(filename).expect("cannot open certificate file");
    let mut reader = ::std::io::BufReader::new(certfile);
    rustls::internal::pemfile::certs(&mut reader).unwrap()
}

fn load_private_key(filename: &Path) -> rustls::PrivateKey {
    use ::std::fs;
    use ::std::io::BufReader;
    let rsa_keys = {
        let keyfile = fs::File::open(filename)
            .expect("cannot open private key file");
        let mut reader = BufReader::new(keyfile);
        rustls::internal::pemfile::rsa_private_keys(&mut reader)
            .expect("file contains invalid rsa private key")
    };

    let pkcs8_keys = {
        let keyfile = fs::File::open(filename)
            .expect("cannot open private key file");
        let mut reader = BufReader::new(keyfile);
        rustls::internal::pemfile::pkcs8_private_keys(&mut reader)
            .expect("file contains invalid pkcs8 private key (encrypted keys not supported)")
    };

    // prefer to load pkcs8 keys
    if !pkcs8_keys.is_empty() {
        pkcs8_keys[0].clone()
    } else {
        assert!(!rsa_keys.is_empty());
        rsa_keys[0].clone()
    }
}

fn make_server_tls(project_root: &Path) -> rustls::ServerConfig {
    let rocket_cfg: RocketConfig = ::toml::from_slice(
        &::std::fs::read(project_root.join("Rocket.toml"))
            .expect("couldn't open project Rocket.toml"),
    )
    .unwrap();
    println!("{:?}", rocket_cfg);
    let mut tls_config = rustls::ServerConfig::new(rustls::NoClientAuth::new());
    let certs = load_certs(&rocket_cfg.global.tls.certs);
    let privkey = load_private_key(&rocket_cfg.global.tls.key);
    tls_config.set_single_cert(certs, privkey).expect("bad certificates/private key");
    // tls_config.set_persistence(rustls::ServerSessionMemoryCache::new(256));

    tls_config
}

// TODO: Make this use Tokio instead.
// It's a royal mess without it.
#[::tokio::main]
pub(crate) async fn start(opt: Opt, bins: Binaries) -> ::anyhow::Result<()> {
    // First, let's set up TLS.
    let tls_config = ::std::sync::Arc::new(make_server_tls(&opt.project_root));
    let tls_acceptor = tokio_rustls::TlsAcceptor::from(tls_config);

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

    let mut listener = ::tokio::net::TcpListener::bind("0.0.0.0:9000")
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
        let acceptor = tls_acceptor.clone();
        // Give each spawned task its own receiver.
        let mut rx = rx.clone();
        ::tokio::spawn(async move {
            ::foretry::async_try! { _, ::anyhow::Error | {
                let stream = acceptor.accept(stream).await?;
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
