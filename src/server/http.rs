//! The network API: `POST /v1/<method>` with JSON, `GET /v1/listen` for SSE.

use crate::engine::access::Caller;
use crate::engine::{agents, catalog, query, subs, users, write, Db};
use crate::error::{ApiError, ApiResult};
use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

pub type Body = BoxBody<Bytes, Infallible>;

pub fn full(b: impl Into<Bytes>) -> Body {
    Full::new(b.into()).boxed()
}

pub fn json_response(status: u16, v: &Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header("cache-control", "no-store")
        .body(full(serde_json::to_vec(v).unwrap()))
        .unwrap()
}

pub fn error_response(e: &ApiError) -> Response<Body> {
    json_response(e.status, &json!({"error": {"code": e.code, "message": e.message}}))
}

pub struct Server {
    pub db: Arc<Db>,
    pub login_slots: Semaphore,
    /// Becomes true when the server stops: connections finish the request in hand, listen streams end.
    pub stop: tokio::sync::watch::Receiver<bool>,
}

/// Resolves once the server is stopping. (The value `wait_for` returns holds a lock; it is dropped here,
/// so the future stays sendable between threads.)
pub async fn stopped(stop: &mut tokio::sync::watch::Receiver<bool>) {
    let _ = stop.wait_for(|s| *s).await;
}

/// Serves one connection until it ends or the server stops. On a stop, the request in progress still
/// gets its answer before the connection closes, so a client is never left without one for work done.
macro_rules! until_stopped {
    ($conn:expr, $stop:expr) => {{
        let conn = $conn;
        tokio::pin!(conn);
        let mut stop = $stop;
        tokio::select! {
            r = conn.as_mut() => r,
            _ = $crate::server::http::stopped(&mut stop) => {
                conn.as_mut().graceful_shutdown();
                conn.await
            }
        }
    }};
}

pub async fn serve(
    srv: Arc<Server>,
    listener: TcpListener,
    tls: Option<Arc<rustls::ServerConfig>>,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    let acceptor = tls.map(tokio_rustls::TlsAcceptor::from);
    loop {
        let (stream, peer) = tokio::select! {
            r = listener.accept() => match r {
                Ok(x) => x,
                Err(e) => { super::log(format!("{}  accept failed: {e}", super::clock())); continue; }
            },
            _ = stop.changed() => break,
        };
        let _ = stream.set_nodelay(true);
        let srv = srv.clone();
        let acceptor = acceptor.clone();
        // counted before the task starts, so the wait below cannot miss a connection accepted just now
        srv.db.stats.connections.fetch_add(1, Ordering::Relaxed);
        tokio::spawn(async move {
            let s2 = srv.clone();
            let stop_c = srv.stop.clone();
            let svc = hyper::service::service_fn(move |req| {
                let s = s2.clone();
                async move { Ok::<_, Infallible>(handle(s, req, peer).await) }
            });
            let mut builder = hyper::server::conn::http1::Builder::new();
            let res = match acceptor {
                Some(a) => {
                    // A TLS handshake always starts with byte 0x16; anything else is plain HTTP
                    // sent to the TLS port, which gets pointed to https:// instead of silence.
                    let mut first = [0u8; 1];
                    let peeked = tokio::time::timeout(std::time::Duration::from_secs(10), stream.peek(&mut first)).await;
                    match peeked {
                        Ok(Ok(1)) if first[0] == 0x16 => match a.accept(stream).await {
                            Ok(tls) => until_stopped!(builder.serve_connection(TokioIo::new(tls), svc), stop_c),
                            Err(_) => Ok(()),
                        },
                        Ok(Ok(1)) => {
                            let redirect = hyper::service::service_fn(|req| async move { Ok::<_, Infallible>(to_https(req)) });
                            builder.keep_alive(false).serve_connection(TokioIo::new(stream), redirect).await
                        }
                        _ => Ok(()),
                    }
                }
                None => until_stopped!(builder.serve_connection(TokioIo::new(stream), svc), stop_c),
            };
            let _ = res;
            srv.db.stats.connections.fetch_sub(1, Ordering::Relaxed);
        });
    }
    // every connection now answers the request in hand and closes; wait for that, up to 30 seconds
    let t0 = std::time::Instant::now();
    while srv.db.stats.connections.load(Ordering::Relaxed) > 0 && t0.elapsed() < std::time::Duration::from_secs(30) {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// Plain HTTP on the TLS port is refused, never redirected: a browser gets a short text page and a
/// program gets the USE_HTTPS error, so a wrong setup (a proxy forwarding plain HTTP) shows itself.
fn to_https(req: Request<Incoming>) -> Response<Body> {
    let host = req.headers().get("host").and_then(|h| h.to_str().ok()).map(str::to_string);
    let path = req.uri().path_and_query().map(|p| p.as_str().to_string()).unwrap_or_else(|| "/".into());
    let method = req.method().clone();
    log_line("-", "plain-http", "", 0, Some("USE_HTTPS"));
    let url = match host {
        Some(h) => format!("https://{h}{path}"),
        None => "https://<this host>:<port>/".to_string(),
    };
    let mut r = if matches!(method, hyper::Method::GET | hyper::Method::HEAD) {
        Response::builder()
            .status(400)
            .header("content-type", "text/plain; charset=utf-8")
            .body(full(format!("This server only speaks HTTPS. Open {url} instead.\n")))
            .unwrap()
    } else {
        json_response(
            400,
            &json!({"error": {"code": "USE_HTTPS", "message": format!("This server only speaks HTTPS; the request was not run. Send it to {url}. Anything sent over plain HTTP (passwords, tokens, keys) may have been seen by others.")}}),
        )
    };
    r.headers_mut().insert("connection", hyper::header::HeaderValue::from_static("close"));
    r
}

fn bearer(req: &Request<Incoming>) -> Option<String> {
    if let Some(h) = req.headers().get("authorization").and_then(|v| v.to_str().ok()) {
        if let Some(t) = h.strip_prefix("Bearer ").or_else(|| h.strip_prefix("bearer ")) {
            return Some(t.trim().to_string());
        }
    }
    // EventSource cannot set headers: ?token= is taken on /v1/listen only
    if req.uri().path() != "/v1/listen" {
        return None;
    }
    req.uri().query().and_then(|q| {
        q.split('&').find_map(|kv| kv.strip_prefix("token=").map(|t| t.to_string()))
    })
}

/// Token -> caller (session or agent key).
pub fn caller_for(db: &Db, token: &str) -> ApiResult<Caller> {
    let bad = || ApiError::unauthorized("Sign in first: send Authorization: Bearer <session token or agent key>.");
    if token.starts_with("dk_") {
        let a = agents::resolve(db, token)?.ok_or_else(|| ApiError::unauthorized("This agent key is unknown, revoked or expired."))?;
        let u = users::get_user(db, &a.user_id)?.ok_or_else(bad)?;
        return Ok(Caller { user_id: u.user_id, username: u.username, level: u.level, admin: false, agent: Some(a) });
    }
    let uid = users::session_user(db, token)?.ok_or_else(|| ApiError::unauthorized("Your session ended or is unknown; sign in again."))?;
    let u = users::get_user(db, &uid)?.ok_or_else(bad)?;
    Ok(Caller { user_id: u.user_id, username: u.username, level: u.level, admin: false, agent: None })
}

async fn handle(srv: Arc<Server>, req: Request<Incoming>, peer: SocketAddr) -> Response<Body> {
    let db = srv.db.clone();
    db.stats.requests.fetch_add(1, Ordering::Relaxed);
    let path = req.uri().path().to_string();
    let ip = peer.ip().to_string();
    if req.method() == hyper::Method::GET && path == "/v1" {
        return json_response(200, &json!({"name": "DADABADA", "version": env!("CARGO_PKG_VERSION")}));
    }
    if req.method() == hyper::Method::GET {
        if let Some(r) = super::ui::serve(&path) {
            return r;
        }
    }
    if req.method() == hyper::Method::GET && path == "/v1/listen" {
        let token = bearer(&req).unwrap_or_default();
        let last = req
            .headers()
            .get("last-event-id")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| u64::from_str_radix(s.trim(), 16).ok());
        let d2 = db.clone();
        let c = tokio::task::spawn_blocking(move || caller_for(&d2, &token)).await.unwrap();
        return match c {
            Ok(c) => {
                log_line(&c.who(), "listen", "", 0, None);
                super::listen::stream(db, c, last, srv.stop.clone())
            }
            Err(e) => error_response(&e),
        };
    }
    let Some(method) = path.strip_prefix("/v1/").map(String::from) else {
        return error_response(&ApiError::not_found(format!("Nothing at {path}. Calls are POST /v1/<method>.")));
    };
    if req.method() != hyper::Method::POST {
        return error_response(&ApiError::new(405, "METHOD", "Use POST for /v1/<method>."));
    }
    let token = bearer(&req);
    let bytes = match req.into_body().collect().await {
        Ok(b) => b.to_bytes(),
        Err(e) => return error_response(&ApiError::invalid(format!("Could not read the request body: {e}"))),
    };
    let body: Value = if bytes.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(e) => return error_response(&ApiError::invalid(format!("The body is not valid JSON: {e}"))),
        }
    };
    let started = std::time::Instant::now();

    if method == "adminInfo" {
        let d2 = db.clone();
        let v = tokio::task::spawn_blocking(move || super::web_admin::info(&d2)).await.unwrap();
        return json_response(200, &v);
    }
    if method == "adminLogin" {
        let pw = body["password"].as_str().unwrap_or("").to_string();
        let _slot = srv.login_slots.acquire().await;
        let d2 = db.clone();
        let ip2 = ip.clone();
        let r = tokio::task::spawn_blocking(move || super::web_admin::login(&d2, &pw, &ip2)).await.unwrap();
        log_line("admin(web)", "adminLogin", "", started.elapsed().as_millis(), r.as_ref().err().map(|e| e.code));
        return match r {
            Ok(v) => json_response(200, &v),
            Err(e) => error_response(&e),
        };
    }
    if method == "adminLogout" || method == "admin" {
        let token = token.unwrap_or_default();
        let d2 = db.clone();
        let m2 = method.clone();
        let b2 = body.clone();
        let r = tokio::task::spawn_blocking(move || -> ApiResult<Value> {
            if !super::web_admin::check(&d2, &token) {
                return Err(ApiError::unauthorized("Sign in as admin first (your admin session may have ended)."));
            }
            if m2 == "adminLogout" {
                return Ok(super::web_admin::logout(&d2, &token));
            }
            let cmd = b2.get("cmd").and_then(Value::as_str).unwrap_or("");
            if cmd.starts_with("web_password") {
                return Err(ApiError::denied("Only the operating system user can change the web admin password (dadabada admin web-password)."));
            }
            super::admin_sock::admin(&d2, &b2)
        })
        .await
        .unwrap_or_else(|e| Err(ApiError::internal(e.to_string())));
        let what = body.get("cmd").and_then(Value::as_str).unwrap_or(&method).to_string();
        // the Overview page's log poll is not itself logged, or it would fill the log it shows
        if what != "logs" {
            // admin commands carry the record or query body under "body"
            let inner = body.get("body").unwrap_or(&body);
            log_line("admin(web)", &what, &collection_of(inner), started.elapsed().as_millis(), r.as_ref().err().map(|e| e.code));
        }
        return match r {
            Ok(v) => json_response(200, &v),
            Err(e) => error_response(&e),
        };
    }

    if method == "login" {
        let user = body["username"].as_str().unwrap_or("").to_string();
        let pw = body["password"].as_str().unwrap_or("").to_string();
        if let Err(e) = users::login_gate(&db, &user, &ip) {
            log_line(&user, "login", "", started.elapsed().as_millis(), Some(e.code));
            return error_response(&e);
        }
        // password checks share a pool the size of the CPU count
        let _slot = srv.login_slots.acquire().await;
        let d2 = db.clone();
        let ip2 = ip.clone();
        let u2 = user.clone();
        let r = tokio::task::spawn_blocking(move || users::login(&d2, &u2, &pw, &ip2)).await.unwrap();
        log_line(&user, "login", "", started.elapsed().as_millis(), r.as_ref().err().map(|e| e.code));
        return match r {
            Ok(v) => json_response(200, &v),
            Err(e) => error_response(&e),
        };
    }

    let Some(token) = token else {
        return error_response(&ApiError::unauthorized("Sign in first: send Authorization: Bearer <session token or agent key>."));
    };
    let d2 = db.clone();
    let m2 = method.clone();
    // the log line needs only the collection, so the body itself moves to the worker thread uncopied
    let coll = collection_of(&body);
    let r = tokio::task::spawn_blocking(move || -> (String, ApiResult<Value>) {
        let c = match caller_for(&d2, &token) {
            Ok(c) => c,
            Err(e) => return (String::new(), Err(e)),
        };
        (c.who(), dispatch(&d2, &c, &m2, &body, &ip, &token))
    })
    .await
    .unwrap_or_else(|e| (String::new(), Err(ApiError::internal(format!("request failed: {e}")))));
    let (who, res) = r;
    match res {
        Ok(v) => {
            log_line(&who, &method, &coll, started.elapsed().as_millis(), None);
            json_response(200, &v)
        }
        Err(e) => {
            log_line(&who, &method, &coll, started.elapsed().as_millis(), Some(e.code));
            error_response(&e)
        }
    }
}

fn collection_of(body: &Value) -> String {
    let pick = |v: &Value| match v {
        Value::String(s) => Some(s.clone()),
        Value::Object(o) => o.get("name").and_then(Value::as_str).map(String::from),
        _ => None,
    };
    body.pointer("/config/collection")
        .and_then(pick)
        .or_else(|| body.pointer("/query/collection").and_then(pick))
        .or_else(|| body.get("collection").and_then(pick))
        .unwrap_or_default()
}

pub fn log_line(who: &str, method: &str, coll: &str, ms: u128, err: Option<&str>) {
    let who = if who.is_empty() { "-" } else { who };
    match err {
        None => super::log(format!("{}  {:<12} {:<28} {:<16} {} ms", super::clock(), who, method, coll, ms)),
        Some(e) => super::log(format!("{}  {:<12} {:<28} {:<16} {} ms  {}", super::clock(), who, method, coll, ms, e)),
    }
}

/// Methods that save, change or remove records: while one runs, a postRecords request saves its records one
/// at a time instead of in groups (see engine::write::post_records).
const RECORD_WRITES: &[&str] = &["postRecord", "postRecords", "deleteRecords", "grantPrivateRecordAccess", "removePrivateRecordAccess"];

pub fn dispatch(db: &Db, c: &Caller, method: &str, body: &Value, ip: &str, token: &str) -> ApiResult<Value> {
    let _writing = RECORD_WRITES.contains(&method).then(|| db.writing());
    let user_arg = || body.get("user_id").and_then(Value::as_str).map(String::from);
    match method {
        "logout" => users::logout(db, token),
        "whoami" => users::whoami(db, c),
        "postRecord" => write::post_record(db, c, body, ip),
        "postRecords" => write::post_records(db, c, body, ip),
        "getRecords" => query::get_records(db, c, body),
        "deleteRecords" => write::delete_records(db, c, body),
        "getCollections" => catalog::get_collections(db, c, body),
        "getIndexes" => catalog::get_indexes(db, c, body),
        "getTags" => catalog::get_tags(db, c, body),
        "getUniqueId" => catalog::get_unique_id(db, c, body),
        "grantPrivateRecordAccess" => write::grant(db, c, body, true),
        "removePrivateRecordAccess" => write::grant(db, c, body, false),
        "listPrivateRecordAccess" => write::list_grants(db, c, body),
        "requestPrivateRecordAccessKey" => write::request_private_key(db, c, body),
        "subscribe" => subs::subscribe(db, c, body),
        "unsubscribe" => subs::unsubscribe(db, c, body),
        "blockSubscriber" => subs::block(db, c, body, true),
        "unblockSubscriber" => subs::block(db, c, body, false),
        "getSubscriptions" => subs::get_subscriptions(db, c, body),
        "getFeed" => subs::get_feed(db, c, body),
        "createAgentKey" => agents::create(db, c, body),
        "listAgentKeys" => {
            let l = agents::list(db, c, user_arg().as_deref())?;
            Ok(json!({"list": l.iter().map(|a| a.json()).collect::<Vec<_>>()}))
        }
        "revokeAgentKey" => {
            let id = body.get("key_id").and_then(Value::as_str).ok_or_else(|| ApiError::invalid("\"key_id\" is required."))?;
            Ok(agents::revoke(db, c, id)?.json())
        }
        _ => Err(ApiError::not_found(format!("There is no method \"{method}\"."))),
    }
}

pub fn status_line(db: &Db, last_req: &mut u64, secs: u64) -> String {
    let r = db.stats.requests.load(Ordering::Relaxed);
    let rate = (r - *last_req) as f64 / secs as f64;
    *last_req = r;
    format!(
        "[outbox {} · req/s {:.1} · conns {}]",
        db.outbox_depth(),
        rate,
        db.stats.connections.load(Ordering::Relaxed)
    )
}
