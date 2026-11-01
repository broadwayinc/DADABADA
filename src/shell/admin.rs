//! `ddbd admin`: talks to the running server over `<data>/admin.sock`.

use super::{print_list, read_line_plain, split, Args};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

struct Admin {
    w: UnixStream,
    r: BufReader<UnixStream>,
    last: Option<(String, Value)>,
}

impl Admin {
    fn call(&mut self, v: Value) -> Result<Value, String> {
        let mut s = v.to_string();
        s.push('\n');
        self.w.write_all(s.as_bytes()).map_err(|e| format!("The server closed the admin socket: {e}"))?;
        let mut line = String::new();
        self.r.read_line(&mut line).map_err(|e| e.to_string())?;
        if line.is_empty() {
            return Err("The server closed the admin socket.".into());
        }
        let out: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        if let Some(e) = out.get("error") {
            return Err(format!("{}: {}", e["code"].as_str().unwrap_or("ERROR"), e["message"].as_str().unwrap_or("")));
        }
        Ok(out["ok"].clone())
    }
}

const HELP: &str = "Admin commands:
  user add NAME [--level NN] [--password P]   add a user (asks for the password if not given)
  user list | user info NAME | user level NAME NN
  user del NAME [--yes]                        delete a user and everything they uploaded
  records [--collection C] [--user NAME] [--level NN] [--limit N] [--asc] [--json]
  record get ID | record del ID
  more                                         next page of the last listing
  query '<getRecords JSON>'                    run any query with admin rights
  collections [>= text | <= text | = text]
  indexes COLLECTION [--order BY] | tags COLLECTION | uniques [>= text | <= text]
  agents [--user NAME] | agent revoke KEY_ID
  status | settings | backup DIR | rebuild-loose | secret rotate
  web-password [--password P]                  turn on the web admin UI (or change its password)
  web-password off                             turn the web admin UI off and sign out its sessions
  export FILE [--collection C] [--user NAME] | import FILE [--skip-existing | --replace]
  help | exit";

/// `ddbd stop`: asks the server over the admin socket to stop cleanly, then waits until its process has
/// ended. With `none_ok` (for `restart`), a server that is not running is not an error: returns false.
pub fn stop(data: &Path, none_ok: bool) -> Result<bool, String> {
    let sock = crate::server::admin_sock::socket_path(data);
    let w = match UnixStream::connect(&sock) {
        Ok(w) => w,
        Err(_) if none_ok => {
            println!("No DADABADA was running for {} (no admin socket at {}).", data.display(), sock.display());
            return Ok(false);
        }
        Err(e) => return Err(format!("No DADABADA is running for {} (could not open {}: {e}).", data.display(), sock.display())),
    };
    let r = BufReader::new(w.try_clone().map_err(|e| e.to_string())?);
    let mut a = Admin { w, r, last: None };
    let res = a.call(json!({"cmd": "shutdown"}))?;
    let pid = res["pid"].as_i64().unwrap_or(0) as libc::pid_t;
    println!("Stopping DADABADA (pid {pid}): it finishes the requests in progress first...");
    // the server owns this user's process, so signal 0 tells whether it is still there; an ended process
    // whose parent has not collected it yet (a zombie) still answers signal 0, so its state is checked too
    while pid > 0 && unsafe { libc::kill(pid, 0) } == 0 && !ended(pid) {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    println!("Stopped.");
    Ok(true)
}

/// True when the process has exited but its parent has not collected it yet (state Z in /proc).
fn ended(pid: libc::pid_t) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|st| st.rsplit(')').next().map(|rest| rest.trim_start().starts_with('Z')))
        .unwrap_or(false)
}

pub fn run(data: &Path, command: Vec<String>) -> Result<(), String> {
    let sock = crate::server::admin_sock::socket_path(data);
    let w = UnixStream::connect(&sock).map_err(|e| {
        format!(
            "Could not reach DADABADA at {} ({e}). Is it running? Start it with: ddbd start --data {}",
            sock.display(),
            data.display()
        )
    })?;
    let r = BufReader::new(w.try_clone().map_err(|e| e.to_string())?);
    let mut a = Admin { w, r, last: None };
    if !command.is_empty() {
        return exec(&mut a, &command);
    }
    let mut rl = rustyline::DefaultEditor::new().map_err(|e| e.to_string())?;
    println!("DADABADA admin shell. Type `help` for commands.");
    loop {
        match rl.readline("ddbd admin> ") {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(line);
                if line == "exit" || line == "quit" {
                    break;
                }
                match split(line) {
                    Ok(words) => {
                        if let Err(e) = exec(&mut a, &words) {
                            println!("{e}");
                        }
                    }
                    Err(e) => println!("{e}"),
                }
            }
            Err(_) => break,
        }
    }
    Ok(())
}

fn level_flag(args: &Args) -> Value {
    args.get("level").map(|l| super::level_arg(l)).unwrap_or(Value::Null)
}

fn cond_body(key: &str, rest: &[String]) -> Value {
    match rest {
        [op, text, ..] if matches!(op.as_str(), ">=" | "<=" | "=" | ">" | "<") => json!({key: text, "condition": op}),
        [text] => json!({key: text, "condition": "="}),
        _ => json!({}),
    }
}

fn listing(a: &mut Admin, cmd: &str, extra: Value, body: Value, json_out: bool) -> Result<(), String> {
    let mut req = extra.clone();
    req["cmd"] = json!(cmd);
    req["body"] = body.clone();
    let v = a.call(req)?;
    print_list(&v, json_out);
    if v["end_of_list"].as_bool() == Some(false) {
        let mut next = body;
        next["fetch"]["start_key"] = v["start_key"].clone();
        let mut ex = extra;
        ex["cmd"] = json!(cmd);
        ex["body"] = next;
        a.last = Some((if json_out { "json".into() } else { String::new() }, ex));
    } else {
        a.last = None;
    }
    Ok(())
}

fn exec(a: &mut Admin, words: &[String]) -> Result<(), String> {
    let args = Args::parse(words);
    let p: Vec<&str> = args.pos.iter().map(String::as_str).collect();
    let json_out = args.has("json");
    match p.as_slice() {
        ["help"] | [] => println!("{HELP}"),
        ["user", "add", name] => {
            let pw = match args.get("password") {
                Some(p) => p.to_string(),
                None => read_line_plain(&format!("Password for {name}: ")),
            };
            let u = a.call(json!({"cmd": "user_add", "username": name, "password": pw, "level": level_flag(&args)}))?;
            println!("Added {} (user id {}, level {}).", u["username"].as_str().unwrap_or(""), u["user_id"].as_str().unwrap_or(""), u["access_level"].as_str().unwrap_or(""));
        }
        ["user", "list"] => {
            let v = a.call(json!({"cmd": "user_list"}))?;
            if json_out {
                println!("{}", serde_json::to_string_pretty(&v).unwrap());
                return Ok(());
            }
            println!("{:<20} {:<10} {:<6} {:>8} {:>12} {:>11}", "USERNAME", "USER ID", "LEVEL", "RECORDS", "SUBSCRIBERS", "SUBSCRIBED");
            for u in v.as_array().cloned().unwrap_or_default() {
                println!(
                    "{:<20} {:<10} {:<6} {:>8} {:>12} {:>11}",
                    u["username"].as_str().unwrap_or(""),
                    u["user_id"].as_str().unwrap_or(""),
                    u["access_level"].as_str().unwrap_or(""),
                    u["records"].to_string(),
                    u["subscribers"].to_string(),
                    u["subscribed"].to_string()
                );
            }
        }
        ["user", "info", name] => println!("{}", serde_json::to_string_pretty(&a.call(json!({"cmd": "user_info", "user": name}))?).unwrap()),
        ["user", "level", name, lvl] => {
            let u = a.call(json!({"cmd": "user_level", "user": name, "level": super::level_arg(lvl)}))?;
            println!("{} is now level {}.", u["username"].as_str().unwrap_or(""), u["access_level"].as_str().unwrap_or(""));
        }
        ["user", "del", name] => {
            let u = a.call(json!({"cmd": "user_info", "user": name}))?;
            let uname = u["username"].as_str().unwrap_or("").to_string();
            if !args.has("yes") {
                println!("This deletes user {uname} and the {} records {uname} uploaded.", u["records"]);
                if read_line_plain("Type the username to confirm: ") != uname {
                    println!("Not deleted.");
                    return Ok(());
                }
            }
            let v = a.call(json!({"cmd": "user_del", "user": name}))?;
            println!("Deleted {uname}. Removing records in the background (outbox {}).", v["outbox"]);
        }
        ["records"] => {
            let mut body = json!({"fetch": {"limit": args.get("limit").and_then(|l| l.parse::<u64>().ok()).unwrap_or(50), "ascending": args.has("asc")}});
            if let Some(c) = args.get("collection") {
                body["collection"] = json!(c);
            }
            if let Some(u) = args.get("user") {
                body["user"] = json!(u);
            }
            if let Some(l) = args.get("level") {
                body["access_level"] = super::level_arg(l);
            }
            listing(a, "records", json!({}), body, json_out)?;
        }
        ["more"] => match a.last.take() {
            Some((mode, req)) => {
                let cmd = req["cmd"].as_str().unwrap_or("").to_string();
                let body = req["body"].clone();
                listing(a, &cmd, req, body, mode == "json")?;
            }
            None => println!("Nothing more to show."),
        },
        ["record", "get", id] => println!("{}", serde_json::to_string_pretty(&a.call(json!({"cmd": "record_get", "record_id": id}))?).unwrap()),
        ["record", "del", id] => {
            a.call(json!({"cmd": "record_del", "record_id": id}))?;
            println!("Deleted record {id}.");
        }
        ["query", q] => {
            let body: Value = serde_json::from_str(q).map_err(|e| format!("The query is not JSON: {e}"))?;
            listing(a, "query", json!({}), body, json_out)?;
        }
        ["collections", rest @ ..] => {
            let rest: Vec<String> = rest.iter().map(|s| s.to_string()).collect();
            listing(a, "collections", json!({}), cond_body("collection", &rest), json_out)?;
        }
        ["tags", coll, rest @ ..] => {
            let rest: Vec<String> = rest.iter().map(|s| s.to_string()).collect();
            let mut b = cond_body("tag", &rest);
            b["collection"] = json!(coll);
            listing(a, "tags", json!({}), b, json_out)?;
        }
        ["indexes", coll, rest @ ..] => {
            let rest: Vec<String> = rest.iter().map(|s| s.to_string()).collect();
            let mut b = cond_body("index", &rest);
            b["collection"] = json!(coll);
            if let Some(by) = args.get("order") {
                b["order"] = json!({"by": by});
            }
            listing(a, "indexes", json!({}), b, json_out)?;
        }
        ["uniques", rest @ ..] => {
            let rest: Vec<String> = rest.iter().map(|s| s.to_string()).collect();
            listing(a, "uniques", json!({}), cond_body("unique_id", &rest), json_out)?;
        }
        ["status"] => {
            let v = a.call(json!({"cmd": "status"}))?;
            if let Some(o) = v.as_object() {
                for (k, val) in o {
                    println!("  {k:<20} {}", val.as_str().map(String::from).unwrap_or_else(|| val.to_string()));
                }
            }
        }
        ["web-password", "off"] => {
            let v = a.call(json!({"cmd": "web_password_off"}))?;
            println!("{}", v["message"].as_str().unwrap_or(""));
        }
        ["web-password"] => {
            let pw = match args.get("password") {
                Some(p) => p.to_string(),
                None => read_line_plain("New web admin password (shown as you type, at least 8 characters): "),
            };
            let v = a.call(json!({"cmd": "web_password", "password": pw}))?;
            println!("{}", v["message"].as_str().unwrap_or(""));
        }
        ["settings"] => {
            let v = a.call(json!({"cmd": "settings_get"}))?;
            println!("Settings file: {}", v["file"].as_str().unwrap_or(""));
            if let Some(o) = v["running"].as_object() {
                for (k, val) in o {
                    let saved = &v["saved"][k];
                    let note = if saved != val { format!("  (saved {saved}, applies on next start)") } else { String::new() };
                    println!("  {k:<28} {val}{note}");
                }
            }
        }
        ["backup", dir] => {
            a.call(json!({"cmd": "backup", "dir": dir}))?;
            println!("Backup written to {dir}.");
        }
        ["agents"] => {
            let mut req = json!({"cmd": "agents"});
            if let Some(u) = args.get("user") {
                req["user"] = json!(u);
            }
            let v = a.call(req)?;
            for k in v.as_array().cloned().unwrap_or_default() {
                println!(
                    "{}  user {}  {}  level≤{}  {}{}  collections:{}",
                    k["key_id"].as_str().unwrap_or(""),
                    k["user_id"].as_str().unwrap_or(""),
                    k["name"].as_str().unwrap_or(""),
                    k["max_level"].as_str().unwrap_or("any"),
                    if k["read_only"].as_bool() == Some(true) { "read-only" } else { "read-write" },
                    if k["private"].as_bool() == Some(true) { " private" } else { "" },
                    k["collections"].as_array().map(|c| c.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(",")).filter(|s| !s.is_empty()).unwrap_or_else(|| "all".into()),
                );
            }
        }
        ["agent", "revoke", id] => {
            a.call(json!({"cmd": "agent_revoke", "key_id": id}))?;
            println!("Revoked agent key {id}.");
        }
        ["secret", "rotate"] => {
            if !args.has("yes") && read_line_plain("This cancels every private access key. Type rotate to confirm: ") != "rotate" {
                println!("Not changed.");
                return Ok(());
            }
            let v = a.call(json!({"cmd": "secret_rotate"}))?;
            println!("{}", v["message"].as_str().unwrap_or(""));
        }
        ["rebuild-loose"] => {
            let v = a.call(json!({"cmd": "rebuild_loose"}))?;
            println!("Rebuilt loose keys for {} records ({} removed, {} written).", v["records"], v["removed_keys"], v["written_keys"]);
        }
        ["export", file] => {
            let file = std::fs::canonicalize(Path::new(file).parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")))
                .map(|d| d.join(Path::new(file).file_name().unwrap_or_default()))
                .map_err(|e| format!("The folder for {file} does not exist: {e}"))?;
            let mut req = json!({"cmd": "export", "file": file.display().to_string()});
            if let Some(c) = args.get("collection") {
                req["collection"] = json!(c);
            }
            if let Some(u) = args.get("user") {
                req["user"] = json!(u);
            }
            let v = a.call(req)?;
            println!(
                "Exported {} users, {} records, {} grants, {} subscriptions, {} agent keys to {}.",
                v["users"], v["records"], v["grants"], v["subscriptions"], v["agent_keys"], v["file"].as_str().unwrap_or("")
            );
        }
        ["import", file] => {
            let file = std::fs::canonicalize(file).map_err(|e| format!("Could not find {file}: {e}"))?;
            let v = a.call(json!({"cmd": "import", "file": file.display().to_string(), "replace": args.has("replace")}))?;
            println!(
                "Imported {} users, {} records, {} grants, {} subscriptions, {} agent keys ({} skipped, {} failed). Catalog counters are updating in the background.",
                v["users"], v["records"], v["grants"], v["subscriptions"], v["agent_keys"], v["skipped"], v["failed_count"]
            );
            for f in v["failed"].as_array().cloned().unwrap_or_default() {
                println!("  line {}: {}", f["line"], f["message"].as_str().unwrap_or(""));
            }
        }
        _ => return Err(format!("Unknown admin command: {}. Type `help`.", words.join(" "))),
    }
    Ok(())
}
