//! `dadabada connect`: the user shell. It sends the same JSON calls as any client.

use super::{guess_value, index_expr, print_list, read_line_plain, record_line, split, Args};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::rt::TokioIo;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncWrite};

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

#[derive(Debug)]
struct Pin {
    want: Option<String>,
    seen: Mutex<Option<String>>,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for Pin {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let fp = crate::server::tls::fingerprint(end_entity);
        *self.seen.lock().unwrap() = Some(fp.clone());
        match &self.want {
            Some(w) if w != &fp => Err(rustls::Error::General(format!(
                "the server certificate changed (expected {w}, got {fp})"
            ))),
            _ => Ok(ServerCertVerified::assertion()),
        }
    }
    fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &self.provider.signature_verification_algorithms)
    }
    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.provider.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

fn known_hosts() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".config"));
    base.join("dadabada").join("known_hosts")
}

fn pinned(addr: &str) -> Option<String> {
    let text = std::fs::read_to_string(known_hosts()).ok()?;
    text.lines().find_map(|l| l.split_once(' ').filter(|(a, _)| *a == addr).map(|(_, f)| f.trim().to_string()))
}

fn remember(addr: &str, fp: &str) {
    let p = known_hosts();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let mut text = std::fs::read_to_string(&p).unwrap_or_default();
    text = text.lines().filter(|l| !l.starts_with(&format!("{addr} "))).map(|l| format!("{l}\n")).collect();
    text.push_str(&format!("{addr} {fp}\n"));
    let _ = std::fs::write(&p, text);
}

struct Conn {
    addr: String,
    host: String,
    tls: Option<(Arc<rustls::ClientConfig>, Arc<Pin>)>,
    token: Option<String>,
    rt: tokio::runtime::Runtime,
    last: Option<(String, Value)>,
    last_event: Option<String>,
}

impl Conn {
    fn new(addr: String, no_tls: bool, want: Option<String>) -> Result<Conn, String> {
        let host = addr.rsplit_once(':').map(|(h, _)| h.to_string()).unwrap_or_else(|| addr.clone());
        let tls = if no_tls {
            None
        } else {
            let provider = crate::server::tls::provider();
            let pin = Arc::new(Pin { want, seen: Mutex::new(None), provider: provider.clone() });
            let cfg = rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .map_err(|e| e.to_string())?
                .dangerous()
                .with_custom_certificate_verifier(pin.clone())
                .with_no_client_auth();
            Some((Arc::new(cfg), pin))
        };
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
        Ok(Conn { addr, host, tls, token: None, rt, last: None, last_event: None })
    }

    async fn open(&self) -> Result<Box<dyn Io>, String> {
        let tcp = tokio::net::TcpStream::connect(&self.addr).await.map_err(|e| format!("Could not connect to {}: {e}", self.addr))?;
        let _ = tcp.set_nodelay(true);
        match &self.tls {
            None => Ok(Box::new(tcp)),
            Some((cfg, _)) => {
                let name = ServerName::try_from(self.host.clone()).map_err(|e| format!("Bad host name {}: {e}", self.host))?;
                let s = tokio_rustls::TlsConnector::from(cfg.clone())
                    .connect(name, tcp)
                    .await
                    .map_err(|e| format!("TLS with {} failed: {e}", self.addr))?;
                Ok(Box::new(s))
            }
        }
    }

    fn seen_fingerprint(&self) -> Option<String> {
        self.tls.as_ref().and_then(|(_, p)| p.seen.lock().unwrap().clone())
    }

    fn call(&self, method: &str, body: Value) -> Result<Value, String> {
        self.rt.block_on(async {
            let io = self.open().await?;
            let (mut send, conn) = hyper::client::conn::http1::handshake(TokioIo::new(io)).await.map_err(|e| e.to_string())?;
            tokio::spawn(conn);
            let mut req = Request::post(format!("/v1/{method}")).header("host", &self.host).header("content-type", "application/json");
            if let Some(t) = &self.token {
                req = req.header("authorization", format!("Bearer {t}"));
            }
            let req = req.body(Full::new(Bytes::from(body.to_string()))).map_err(|e| e.to_string())?;
            let resp = send.send_request(req).await.map_err(|e| format!("The request failed: {e}"))?;
            let bytes = resp.into_body().collect().await.map_err(|e| e.to_string())?.to_bytes();
            let v: Value = serde_json::from_slice(&bytes).map_err(|e| format!("The server sent something that is not JSON: {e}"))?;
            if let Some(e) = v.get("error") {
                return Err(format!("{}: {}", e["code"].as_str().unwrap_or("ERROR"), e["message"].as_str().unwrap_or("")));
            }
            Ok(v)
        })
    }

    fn listen(&mut self) -> Result<(), String> {
        let last = self.last_event.clone();
        let r: Result<Option<String>, String> = self.rt.block_on(async {
            let io = self.open().await?;
            let (mut send, conn) = hyper::client::conn::http1::handshake(TokioIo::new(io)).await.map_err(|e| e.to_string())?;
            tokio::spawn(conn);
            let mut req = Request::get("/v1/listen").header("host", &self.host);
            if let Some(t) = &self.token {
                req = req.header("authorization", format!("Bearer {t}"));
            }
            if let Some(l) = &last {
                req = req.header("last-event-id", l);
            }
            let resp = send.send_request(req.body(Full::new(Bytes::new())).unwrap()).await.map_err(|e| e.to_string())?;
            if resp.status() != 200 {
                let b = resp.into_body().collect().await.map_err(|e| e.to_string())?.to_bytes();
                return Err(String::from_utf8_lossy(&b).into_owned());
            }
            println!("Listening for notifications. Ctrl-C to stop.");
            let mut body = resp.into_body();
            let mut buf = String::new();
            let mut last_id = last.clone();
            loop {
                tokio::select! {
                    f = body.frame() => {
                        let Some(Ok(f)) = f else { println!("The server closed the stream."); break; };
                        if let Ok(d) = f.into_data() {
                            buf.push_str(&String::from_utf8_lossy(&d));
                            while let Some(end) = buf.find("\n\n") {
                                let ev: String = buf.drain(..end + 2).collect();
                                let (mut id, mut kind, mut data) = (None, "message".to_string(), String::new());
                                for l in ev.lines() {
                                    if let Some(v) = l.strip_prefix("id: ") { id = Some(v.to_string()); }
                                    if let Some(v) = l.strip_prefix("event: ") { kind = v.to_string(); }
                                    if let Some(v) = l.strip_prefix("data: ") { data.push_str(v); }
                                }
                                if let Some(i) = id { last_id = Some(i); }
                                match kind.as_str() {
                                    "notify" => {
                                        let v: Value = serde_json::from_str(&data).unwrap_or(Value::Null);
                                        println!("{}  {}: {}  (from {}, record {})", crate::server::clock(),
                                            v["title"].as_str().unwrap_or(""), v["body"].as_str().unwrap_or(""),
                                            v["from"].as_str().unwrap_or(""), v["record_id"].as_str().unwrap_or(""));
                                    }
                                    "gap" => println!("(some notifications expired or were skipped: {data})"),
                                    _ => {}
                                }
                            }
                        }
                    }
                    _ = tokio::signal::ctrl_c() => { println!(); break; }
                }
            }
            Ok(last_id)
        });
        self.last_event = r?;
        Ok(())
    }
}

const HELP: &str = "Commands:
  post COLLECTION [--level NN|**] [--index name=value]... [--tags a,b] [--unique ID] [--ref ID]
       [--readonly] [--data JSON] [--source JSON] [--subscription JSON] [--notification \"title|body\"]
  update RECORD_ID [same options as post; --index replaces every index]
  get [COLLECTION] [--index \"name OP value\"] [--by $updated|$uploaded|$referenced_count] [--tag T]
      [--ref ID] [--user USER_ID] [--level NN|**] [--sub USER_ID] [--limit N] [--desc] [--json]
      OP: = > < >= (starts with) <= (ends with); put ~ before it for a loose match; or name lo..hi
  get --id RECORD_ID | get --unique UNIQUE_ID [--key PRIVATE_KEY]
  more | del RECORD_ID...
  collections [>= text | <= text] | indexes COLLECTION [--order BY] | tags COLLECTION | uniques [>= text]
  grant RECORD_ID USER_ID... | revoke RECORD_ID USER_ID... | grants RECORD_ID | key RECORD_ID
  subscribe USER_ID [--feed] [--notify] | unsubscribe USER_ID | block USER_ID | unblock USER_ID
  subscriptions [--subscriber ID] [--subscription ID] | feed | listen
  agent create NAME [--level NN] [--read-only] [--private] [--collections a,b] [--expires 30d]
  agent list | agent revoke KEY_ID
  whoami | raw METHOD 'JSON' | help | exit";

pub fn run(
    addr: String,
    user: Option<String>,
    key: Option<String>,
    password: Option<String>,
    no_tls: bool,
    trust: bool,
    command: Vec<String>,
) -> Result<(), String> {
    let addr = if addr.contains(':') { addr } else { format!("{addr}:7070") };
    let pin = if no_tls { None } else { pinned(&addr) };
    let mut c = Conn::new(addr.clone(), no_tls, pin.clone())?;
    if !no_tls && pin.is_none() {
        // first contact: show the fingerprint before sending anything secret
        c.rt.block_on(c.open()).map(drop)?;
        let fp = c.seen_fingerprint().unwrap_or_default();
        let ok = trust || read_line_plain(&format!("Certificate fingerprint SHA256 {fp} - trust it? (yes/no) ")).trim() == "yes";
        if !ok {
            return Err("Not connected.".into());
        }
        remember(&addr, &fp);
        c = Conn::new(addr.clone(), no_tls, Some(fp))?;
    }
    let who = if let Some(k) = key {
        c.token = Some(k);
        let me = c.call("whoami", json!({}))?;
        format!("{}/{}", me["username"].as_str().unwrap_or(""), me["agent"]["name"].as_str().unwrap_or("agent"))
    } else {
        let u = match user {
            Some(u) => u,
            None => read_line_plain("Username: "),
        };
        let pw = match password {
            Some(p) => p,
            None => read_line_plain("Password: "),
        };
        let v = c.call("login", json!({"username": u, "password": pw}))?;
        c.token = v["token"].as_str().map(String::from);
        if command.is_empty() {
            println!("Signed in as {} (level {}).", v["user"]["username"].as_str().unwrap_or(""), v["user"]["access_level"].as_str().unwrap_or(""));
        }
        u
    };
    if !command.is_empty() {
        let r = exec(&mut c, &command);
        if c.token.as_deref().is_some_and(|t| t.starts_with("s_")) {
            let _ = c.call("logout", json!({}));
        }
        return r;
    }
    let mut rl = rustyline::DefaultEditor::new().map_err(|e| e.to_string())?;
    loop {
        match rl.readline(&format!("{who}> ")) {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(line);
                if matches!(line, "exit" | "quit" | "logout") {
                    break;
                }
                match split(line) {
                    Ok(words) => {
                        if let Err(e) = exec(&mut c, &words) {
                            println!("{e}");
                        }
                    }
                    Err(e) => println!("{e}"),
                }
            }
            Err(_) => break,
        }
    }
    if c.token.as_deref().is_some_and(|t| t.starts_with("s_")) {
        let _ = c.call("logout", json!({}));
    }
    Ok(())
}

fn json_arg(args: &Args, k: &str) -> Result<Option<Value>, String> {
    args.get(k).map(|s| serde_json::from_str(s).map_err(|e| format!("--{k} is not JSON: {e}"))).transpose()
}

fn post_config(args: &Args, collection: Option<&str>) -> Result<Value, String> {
    let mut cfg = json!({});
    if let Some(c) = collection {
        cfg["collection"] = json!({"name": c});
    }
    if let Some(l) = args.get("level") {
        if cfg.get("collection").is_none() {
            cfg["collection"] = json!({});
        }
        cfg["collection"]["access_level"] = super::level_arg(l);
    }
    if let Some(s) = json_arg(args, "subscription")? {
        if cfg.get("collection").is_none() {
            cfg["collection"] = json!({});
        }
        cfg["collection"]["subscription"] = s;
    }
    let idx = args.all("index");
    if !idx.is_empty() {
        let mut list = vec![];
        for i in idx {
            let (n, v) = i.split_once('=').ok_or_else(|| format!("--index {i}: use name=value"))?;
            list.push(json!({"name": n.trim(), "value": guess_value(v)}));
        }
        cfg["index"] = json!(list);
    }
    if args.has("no-index") {
        cfg["index"] = Value::Null;
    }
    if let Some(t) = args.get("tags") {
        cfg["tags"] = if t.is_empty() { Value::Null } else { json!(t.split(',').map(str::trim).collect::<Vec<_>>()) };
    }
    if let Some(u) = args.get("unique") {
        cfg["unique_id"] = if u.is_empty() { Value::Null } else { json!(u) };
    }
    if let Some(r) = args.get("ref") {
        cfg["reference"] = if r.is_empty() { Value::Null } else { json!(r) };
    }
    if args.has("readonly") {
        cfg["readonly"] = json!(true);
    }
    if let Some(s) = json_arg(args, "source")? {
        cfg["source"] = s;
    }
    if let Some(n) = args.get("notification") {
        let (t, b) = n.split_once('|').unwrap_or((n, ""));
        cfg["notification"] = json!({"title": t, "body": b});
    }
    Ok(cfg)
}

fn listing(c: &mut Conn, method: &str, body: Value, json_out: bool) -> Result<(), String> {
    let v = c.call(method, body.clone())?;
    print_list(&v, json_out);
    if v["end_of_list"].as_bool() == Some(false) {
        let mut next = body;
        next["fetch"]["start_key"] = v["start_key"].clone();
        next["_json"] = json!(json_out);
        c.last = Some((method.to_string(), next));
    } else {
        c.last = None;
    }
    Ok(())
}

fn cond_body(key: &str, rest: &[&str]) -> Value {
    match rest {
        [op, text, ..] if matches!(*op, ">=" | "<=" | "=" | ">" | "<") => json!({key: text, "condition": op}),
        [text] => json!({key: text, "condition": "="}),
        _ => json!({}),
    }
}

fn exec(c: &mut Conn, words: &[String]) -> Result<(), String> {
    let args = Args::parse(words);
    let p: Vec<&str> = args.pos.iter().map(String::as_str).collect();
    let json_out = args.has("json");
    match p.as_slice() {
        ["help"] | [] => println!("{HELP}"),
        ["whoami"] => println!("{}", serde_json::to_string_pretty(&c.call("whoami", json!({}))?).unwrap()),
        ["post", coll] => {
            let data = json_arg(&args, "data")?.unwrap_or(Value::Null);
            let r = c.call("postRecord", json!({"data": data, "config": post_config(&args, Some(coll))?}))?;
            if json_out {
                println!("{}", serde_json::to_string_pretty(&r).unwrap());
            } else {
                println!("Saved record {}", r["record_id"].as_str().unwrap_or(""));
            }
        }
        ["update", id] => {
            let mut cfg = post_config(&args, args.get("collection"))?;
            cfg["record_id"] = json!(id);
            let mut body = json!({"config": cfg});
            if let Some(d) = json_arg(&args, "data")? {
                body["data"] = d;
            }
            let r = c.call("postRecord", body)?;
            if json_out {
                println!("{}", serde_json::to_string_pretty(&r).unwrap());
            } else {
                println!("Updated {}", record_line(&r));
            }
        }
        ["get", rest @ ..] => {
            let mut q = json!({});
            if let Some(id) = args.get("id") {
                q["record_id"] = json!(id);
            } else if let Some(u) = args.get("unique") {
                q["unique_id"] = json!(u);
            } else {
                let coll = rest.first().copied().or(args.get("collection")).ok_or("get needs a collection (or --id / --unique).")?;
                let mut col = json!({"name": coll});
                if let Some(l) = args.get("level") {
                    col["access_level"] = super::level_arg(l);
                }
                if let Some(s) = args.get("sub") {
                    col["subscription"] = json!(s);
                }
                q["collection"] = col;
                if let Some(e) = args.get("index") {
                    let mut ix = index_expr(e)?;
                    if args.has("i") || args.has("ignore-case") {
                        ix["match"] = json!("loose");
                    }
                    q["index"] = ix;
                } else if let Some(by) = args.get("by") {
                    q["index"] = json!({"name": by});
                }
                if let Some(t) = args.get("tag") {
                    q["tag"] = json!(t);
                }
                if let Some(r) = args.get("ref") {
                    q["reference"] = json!(r);
                }
                if let Some(u) = args.get("user") {
                    q["reference"] = json!({"user_id": u});
                }
            }
            if let Some(k) = args.get("key") {
                q["private_key"] = json!(k);
            }
            let limit = args.get("limit").and_then(|l| l.parse::<u64>().ok()).unwrap_or(50);
            listing(c, "getRecords", json!({"query": q, "fetch": {"limit": limit, "ascending": !args.has("desc")}}), json_out)?;
        }
        ["more"] => match c.last.take() {
            Some((m, mut body)) => {
                let j = body["_json"].as_bool().unwrap_or(false);
                if let Some(o) = body.as_object_mut() {
                    o.remove("_json");
                }
                listing(c, &m, body, j)?;
            }
            None => println!("Nothing more to show."),
        },
        ["del", ids @ ..] if !ids.is_empty() => {
            let v = c.call("deleteRecords", json!({"record_id": ids}))?;
            for d in v["deleted"].as_array().cloned().unwrap_or_default() {
                println!("Deleted {}", d.as_str().unwrap_or(""));
            }
            for f in v["failed"].as_array().cloned().unwrap_or_default() {
                println!("Not deleted {}: {}", f["record_id"].as_str().unwrap_or(""), f["message"].as_str().unwrap_or(""));
            }
        }
        ["collections", rest @ ..] => listing(c, "getCollections", json!({"query": cond_body("collection", rest)}), json_out)?,
        ["tags", coll, rest @ ..] => {
            let mut q = cond_body("tag", rest);
            q["collection"] = json!(coll);
            listing(c, "getTags", json!({"query": q}), json_out)?
        }
        ["indexes", coll, rest @ ..] => {
            let mut q = cond_body("index", rest);
            q["collection"] = json!(coll);
            if let Some(by) = args.get("order") {
                q["order"] = json!({"by": by});
            }
            listing(c, "getIndexes", json!({"query": q, "fetch": {"ascending": !args.has("desc")}}), json_out)?
        }
        ["uniques", rest @ ..] => listing(c, "getUniqueId", json!({"query": cond_body("unique_id", rest)}), json_out)?,
        ["grant", id, users @ ..] if !users.is_empty() => {
            let v = c.call("grantPrivateRecordAccess", json!({"record_id": id, "user_id": users}))?;
            println!("Granted: {}", v["granted"]);
        }
        ["revoke", id, users @ ..] if !users.is_empty() => {
            let v = c.call("removePrivateRecordAccess", json!({"record_id": id, "user_id": users}))?;
            println!("Removed: {}", v["removed"]);
        }
        ["grants", id] => print_list(&c.call("listPrivateRecordAccess", json!({"record_id": id}))?, true),
        ["key", id] => {
            let mut b = json!({"record_id": id});
            if let Some(r) = args.get("ref") {
                b["reference_id"] = json!(r);
            }
            let v = c.call("requestPrivateRecordAccessKey", b)?;
            println!("Private access key (expires {}): {}", v["expires"], v["private_key"].as_str().unwrap_or(""));
        }
        ["subscribe", u] => {
            let v = c.call("subscribe", json!({"user_id": u, "get_feed": args.has("feed"), "get_notified": args.has("notify")}))?;
            println!("Subscribed to {u} (feed {}, notifications {}).", v["get_feed"], v["get_notified"]);
        }
        ["unsubscribe", u] => println!("{}", c.call("unsubscribe", json!({"user_id": u}))?["message"].as_str().unwrap_or("")),
        ["block", u] => {
            c.call("blockSubscriber", json!({"user_id": u}))?;
            println!("Blocked {u}.");
        }
        ["unblock", u] => {
            c.call("unblockSubscriber", json!({"user_id": u}))?;
            println!("Unblocked {u}.");
        }
        ["subscriptions"] => {
            let mut b = json!({});
            match (args.get("subscriber"), args.get("subscription")) {
                (None, None) => {
                    let me = c.call("whoami", json!({}))?;
                    b["subscriber"] = me["user_id"].clone();
                }
                (s, t) => {
                    if let Some(s) = s {
                        b["subscriber"] = json!(s);
                    }
                    if let Some(t) = t {
                        b["subscription"] = json!(t);
                    }
                }
            }
            listing(c, "getSubscriptions", b, true)?
        }
        ["feed"] => listing(c, "getFeed", json!({"fetch": {"limit": args.get("limit").and_then(|l| l.parse::<u64>().ok()).unwrap_or(50)}}), json_out)?,
        ["listen"] => c.listen()?,
        ["agent", "create", name] => {
            let mut b = json!({"name": name, "read_only": args.has("read-only"), "private": args.has("private")});
            if let Some(l) = args.get("level") {
                b["max_level"] = super::level_arg(l);
            }
            if let Some(cs) = args.get("collections") {
                b["collections"] = json!(cs.split(',').map(str::trim).collect::<Vec<_>>());
            }
            if let Some(e) = args.get("expires") {
                b["expires"] = json!(e);
            }
            let v = c.call("createAgentKey", b)?;
            println!("Agent key {} for \"{}\"", v["key_id"].as_str().unwrap_or(""), v["name"].as_str().unwrap_or(""));
            println!("Key (shown once): {}", v["key"].as_str().unwrap_or(""));
        }
        ["agent", "list"] => print_list(&c.call("listAgentKeys", json!({}))?, true),
        ["agent", "revoke", id] => {
            c.call("revokeAgentKey", json!({"key_id": id}))?;
            println!("Revoked agent key {id}.");
        }
        ["raw", method, body] => {
            let b: Value = serde_json::from_str(body).map_err(|e| format!("The body is not JSON: {e}"))?;
            println!("{}", serde_json::to_string_pretty(&c.call(method, b)?).unwrap());
        }
        ["raw", method] => println!("{}", serde_json::to_string_pretty(&c.call(method, json!({}))?).unwrap()),
        _ => return Err(format!("Unknown command: {}. Type `help`.", words.join(" "))),
    }
    Ok(())
}
