mod codec;
mod config;
mod debug;
mod engine;
mod error;
mod server;
mod shell;
mod store;

use clap::{Parser, Subcommand};
use config::{parse_duration_ms, Config};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "ddbd", version, about = "DADABADA: a lightweight schemaless record database server")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Make a data folder with settings (and a TLS certificate)
    Init {
        #[arg(long, default_value_t = 7070)]
        port: u16,
        #[arg(long, default_value = "0.0.0.0")]
        bind: String,
        #[arg(long, env = "DADABADA_DATA", default_value = "./dadabada-data")]
        data: PathBuf,
        /// Serve HTTPS itself, with a self-signed certificate made now (default: plain HTTP, for running behind HTTPS)
        #[arg(long)]
        tls: bool,
    },
    /// Run the server in this terminal
    Start {
        #[arg(long, env = "DADABADA_DATA", default_value = "./dadabada-data")]
        data: PathBuf,
        /// How long missed notifications are kept (30m, 12h, 1d, 7d)
        #[arg(long)]
        notify_keep: Option<String>,
        /// How long a private access key works (30m, 1h, 1d)
        #[arg(long)]
        private_key_ttl: Option<String>,
        /// Most indexes per record (0 = unlimited)
        #[arg(long)]
        max_indexes: Option<usize>,
        /// Most tags per record (0 = unlimited)
        #[arg(long)]
        max_tags: Option<usize>,
    },
    /// Stop the running server cleanly (only the operating system user who runs it)
    Stop {
        #[arg(long, env = "DADABADA_DATA", default_value = "./dadabada-data")]
        data: PathBuf,
    },
    /// Stop the running server, then start it again in this terminal (same options as start)
    Restart {
        #[arg(long, env = "DADABADA_DATA", default_value = "./dadabada-data")]
        data: PathBuf,
        /// How long missed notifications are kept (30m, 12h, 1d, 7d)
        #[arg(long)]
        notify_keep: Option<String>,
        /// How long a private access key works (30m, 1h, 1d)
        #[arg(long)]
        private_key_ttl: Option<String>,
        /// Most indexes per record (0 = unlimited)
        #[arg(long)]
        max_indexes: Option<usize>,
        /// Most tags per record (0 = unlimited)
        #[arg(long)]
        max_tags: Option<usize>,
    },
    /// Admin shell over the local socket (or one admin command)
    Admin {
        #[arg(long, env = "DADABADA_DATA", default_value = "./dadabada-data")]
        data: PathBuf,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// User shell over the network (or one command)
    Connect {
        /// host:port
        address: String,
        #[arg(long)]
        user: Option<String>,
        /// Use an agent key instead of a username and password
        #[arg(long)]
        key: Option<String>,
        #[arg(long)]
        password: Option<String>,
        /// The server serves HTTPS itself (it was set up with --tls)
        #[arg(long)]
        tls: bool,
        /// Trust the server certificate without asking (and remember it)
        #[arg(long)]
        trust: bool,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Show how values are encoded (key, sort, loose, reverse)
    Debug {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Print the version
    Version,
}

fn main() {
    let cli = Cli::parse();
    if !matches!(cli.cmd, Cmd::Start { .. } | Cmd::Restart { .. }) {
        // command-line tools end quietly when piped into `head`; the server keeps ignoring SIGPIPE
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        }
    }
    let code = match cli.cmd {
        Cmd::Init { port, bind, data, tls } => init(port, bind, &data, tls),
        Cmd::Start { data, notify_keep, private_key_ttl, max_indexes, max_tags } => {
            start(&data, notify_keep, private_key_ttl, max_indexes, max_tags)
        }
        Cmd::Stop { data } => shell::admin::stop(&data, false).map(|_| ()),
        Cmd::Restart { data, notify_keep, private_key_ttl, max_indexes, max_tags } => {
            shell::admin::stop(&data, true).and_then(|_| start(&data, notify_keep, private_key_ttl, max_indexes, max_tags))
        }
        Cmd::Admin { data, command } => shell::admin::run(&data, command),
        Cmd::Connect { address, user, key, password, tls, trust, command } => {
            shell::client::run(address, user, key, password, !tls, trust, command)
        }
        Cmd::Debug { args } => debug::run(&args),
        Cmd::Version => {
            println!("ddbd {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    };
    if let Err(e) = code {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn abs(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn init(port: u16, bind: String, data: &Path, tls: bool) -> Result<(), String> {
    if Config::path(data).exists() {
        return Err(format!("{} is already set up. Start it with: ddbd start --data {}", data.display(), data.display()));
    }
    std::fs::create_dir_all(data).map_err(|e| format!("Could not create {}: {e}", data.display()))?;
    server::tls::set_mode(data, 0o700);
    let data = abs(data);
    let mut cfg = Config { port, bind, tls, ..Config::default() };
    let owner = std::env::var("USER").unwrap_or_else(|_| "this user".into());
    println!("Created data folder {} (owner: {owner}, mode 0700)", data.display());
    if cfg.tls {
        let fp = server::tls::make_self_signed(&data)?;
        cfg.tls_cert = Some(data.join("tls/cert.pem").display().to_string());
        cfg.tls_key = Some(data.join("tls/key.pem").display().to_string());
        cfg.save(&data)?;
        println!("Saved settings to {}", Config::path(&data).display());
        println!("Made a TLS certificate. Clients can pin this fingerprint:\n  SHA256 {fp}");
    } else {
        cfg.save(&data)?;
        println!("Saved settings to {}", Config::path(&data).display());
        println!("TLS is off: DADABADA speaks plain HTTP, so passwords and data travel unencrypted.");
        println!("Run it behind HTTPS (a load balancer or proxy that serves HTTPS), or set it up with --tls.");
    }
    drop(engine::Db::open(&data, cfg)?);
    println!("Next: ddbd start --data {}", data.display());
    println!("Then add a user: ddbd admin --data {} user add <name> --level 00", data.display());
    Ok(())
}

fn start(
    data: &Path,
    notify_keep: Option<String>,
    private_key_ttl: Option<String>,
    max_indexes: Option<usize>,
    max_tags: Option<usize>,
) -> Result<(), String> {
    let data = abs(data);
    let mut cfg = Config::load(&data)?;
    if let Some(v) = notify_keep {
        cfg.notify_keep = v;
    }
    if let Some(v) = private_key_ttl {
        cfg.private_key_ttl = v;
    }
    if let Some(v) = max_indexes {
        cfg.max_indexes = v;
    }
    if let Some(v) = max_tags {
        cfg.max_tags = v;
    }
    for (name, v) in [
        ("notify_keep", &cfg.notify_keep),
        ("private_key_ttl", &cfg.private_key_ttl),
        ("login_max_delay", &cfg.login_max_delay),
    ] {
        if parse_duration_ms(v).is_none() {
            return Err(format!("{name} = \"{v}\" is not a duration. Use forms like 30m, 12h, 1d."));
        }
    }
    let tls = if cfg.tls {
        let cert = cfg.tls_cert.clone().map(PathBuf::from).unwrap_or_else(|| data.join("tls/cert.pem"));
        let key = cfg.tls_key.clone().map(PathBuf::from).unwrap_or_else(|| data.join("tls/key.pem"));
        Some(server::tls::load(&cert, &key)?)
    } else {
        None
    };
    let db = Arc::new(engine::Db::open(&data, cfg.clone())?);
    let users = engine::users::list_users(&db).map_err(|e| e.message)?.len();
    let (colls, recs) = engine::catalog::collection_count(&db).map_err(|e| e.message)?;
    let sock = server::admin_sock::socket_path(&data);
    let owner = std::env::var("USER").unwrap_or_else(|_| "this user".into());

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().map_err(|e| e.to_string())?;
    let result = rt.block_on(async {
        let listener = tokio::net::TcpListener::bind((cfg.bind.as_str(), cfg.port))
            .await
            .map_err(|e| format!("Could not listen on {}:{}: {e}", cfg.bind, cfg.port))?;
        server::admin_sock::serve(db.clone(), sock.clone()).await?;
        println!("DADABADA {}  (schemaless, lexicographic, single binary)", env!("CARGO_PKG_VERSION"));
        println!("  data     {}", data.display());
        println!("  network  {}:{} ({})", cfg.bind, cfg.port, if tls.is_some() { "TLS" } else { "plain HTTP" });
        if let Some((_, fp)) = &tls {
            println!("  cert     SHA256 {fp}");
        }
        println!("  admin    {} (only OS user {owner})", sock.display());
        println!(
            "  limits   indexes per record: {} · tags per record: {} · notifications kept {} · private keys last {}",
            if cfg.max_indexes == 0 { "unlimited".to_string() } else { cfg.max_indexes.to_string() },
            if cfg.max_tags == 0 { "unlimited".to_string() } else { cfg.max_tags.to_string() },
            cfg.notify_keep,
            cfg.private_key_ttl
        );
        println!("  users {users} · records {recs} · collections {colls}");
        let lv = server::admin_sock::stored_loose_version(&db);
        if lv != codec::loose::LOOSE_VERSION {
            println!(
                "  NOTE: loose search rules changed (stored {lv}, this build {}). Run: ddbd admin --data {} rebuild-loose",
                codec::loose::LOOSE_VERSION,
                data.display()
            );
        }
        server::log(format!("{}  Ready. DADABADA {} on {}:{} ({})", server::clock(), env!("CARGO_PKG_VERSION"), cfg.bind, cfg.port, if tls.is_some() { "TLS" } else { "plain HTTP" }));

        let worker = engine::worker::spawn(db.clone());
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let srv = Arc::new(server::http::Server {
            db: db.clone(),
            login_slots: tokio::sync::Semaphore::new(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2)),
            stop: stop_rx.clone(),
        });
        let serving = tokio::spawn(server::http::serve(srv, listener, tls.map(|t| t.0), stop_rx));

        let d2 = db.clone();
        let status = tokio::spawn(async move {
            let mut last_req = 0u64;
            let mut last_line = String::new();
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(10));
            tick.tick().await;
            loop {
                tick.tick().await;
                let d3 = d2.clone();
                let mut lr = last_req;
                let (line, lr2) = tokio::task::spawn_blocking(move || {
                    let l = server::http::status_line(&d3, &mut lr, 10);
                    (l, lr)
                })
                .await
                .unwrap_or_default();
                last_req = lr2;
                if line != last_line {
                    server::log(line.clone());
                    last_line = line;
                }
            }
        });

        // Ctrl-C (SIGINT), SIGTERM (systemctl stop, kill) and the `shutdown` admin command all stop cleanly
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).map_err(|e| e.to_string())?;
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
            _ = db.shutdown.notified() => {
                // let the answer to the command reach the caller before connections close
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            }
        }
        println!("\n{}: refusing new requests, finishing the ones in progress...", if db.restart.load(Ordering::SeqCst) { "Restarting" } else { "Stopping" });
        let _ = stop_tx.send(true);
        status.abort();
        let _ = serving.await;
        db.stopping.store(true, Ordering::SeqCst);
        db.wake_worker();
        let _ = tokio::task::spawn_blocking(move || worker.join()).await;
        Ok::<(), String>(())
    });
    let _ = std::fs::remove_file(&sock);
    rt.shutdown_timeout(std::time::Duration::from_secs(2));
    // what is in memory goes to disk now, so the next start opens at once instead of replaying the log
    if let Err(e) = db.store.flush_all() {
        println!("Could not write everything to disk before stopping ({}); the next start replays the log.", e.message);
    }
    let left = db.outbox_depth();
    let restart = db.restart.load(Ordering::SeqCst);
    drop(db);
    if result.is_ok() {
        if left > 0 {
            println!("Stopped. {left} background jobs wait in the outbox; they continue at the next start.");
        } else {
            println!("Stopped.");
        }
    }
    if result.is_ok() && restart {
        return restart_in_place(&data);
    }
    result
}

/// Starts the server again in this same process: same pid, same terminal and output, same options.
/// The program file is looked up again, so a new build at the same path is the one that starts.
fn restart_in_place(data: &Path) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let mut args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let prog = args.remove(0);
    // a server started by `ddbd restart` comes back as `start`, so it does not try to stop itself
    if let Some(a) = args.first_mut().filter(|a| *a == "restart") {
        *a = "start".into();
    }
    println!("Starting again...");
    let err = std::process::Command::new(&prog).args(&args).exec();
    Err(format!("Could not start again ({err}). Start it by hand: ddbd start --data {}", data.display()))
}
