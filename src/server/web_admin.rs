//! Web admin: a password the OS user sets, admin sessions, settings, records on a user's behalf.

use crate::config::{parse_duration_ms, Config};
use crate::engine::access::Caller;
use crate::engine::users::{self, hash_password, sha_hex};
use crate::engine::{write, Db};
use crate::error::{ApiError, ApiResult};
use crate::store::CF_META;
use argon2::password_hash::{PasswordHash, PasswordVerifier};
use argon2::Argon2;
use serde_json::{json, Map, Value};

const PW_KEY: &[u8] = b"web_admin";
/// Throttle bucket for the admin password (a name no user can have).
const THROTTLE_NAME: &str = "\u{0}admin";

/// Settings that apply while running; the rest apply on the next start.
pub const LIVE: &[&str] = &[
    "session_idle_hours",
    "notify_keep",
    "private_key_ttl",
    "login_free_attempts",
    "login_max_delay",
    "max_indexes",
    "max_tags",
    "admin_session_idle_minutes",
];
pub const ON_RESTART: &[&str] = &["port", "bind", "tls", "tls_cert", "tls_key", "rocksdb.block_cache_mb", "rocksdb.blob_min_kb"];

pub fn enabled(db: &Db) -> bool {
    matches!(db.store.get(CF_META, PW_KEY), Ok(Some(_)))
}

/// OS user only (admin socket): set or replace the web admin password.
pub fn set_password(db: &Db, pw: &str) -> ApiResult<Value> {
    if pw.chars().count() < 8 {
        return Err(ApiError::invalid("The web admin password must be at least 8 characters."));
    }
    db.store.put(CF_META, PW_KEY, hash_password(pw)?.as_bytes())?;
    db.admin_sessions.lock().unwrap().clear();
    Ok(json!({"message": "Web admin is on. Open the server address in a browser and sign in with this password. Existing web admin sessions were signed out."}))
}

/// OS user only: turn web admin off and end every web admin session.
pub fn turn_off(db: &Db) -> ApiResult<Value> {
    db.store.delete(CF_META, PW_KEY)?;
    db.admin_sessions.lock().unwrap().clear();
    Ok(json!({"message": "Web admin is off. Every web admin session was signed out."}))
}

pub fn info(db: &Db) -> Value {
    json!({"name": "DADABADA", "version": env!("CARGO_PKG_VERSION"), "web_admin": enabled(db)})
}

pub fn login(db: &Db, password: &str, ip: &str) -> ApiResult<Value> {
    users::login_gate(db, THROTTLE_NAME, ip)?;
    let Some(hash) = db.store.get(CF_META, PW_KEY)? else {
        return Err(ApiError::denied(
            "Web admin is off. On the server, the operating system user turns it on with: ddbd admin web-password",
        ));
    };
    let hash = String::from_utf8_lossy(&hash).into_owned();
    let ok = PasswordHash::new(&hash).map(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok()).unwrap_or(false);
    if !ok {
        db.throttle.lock().unwrap().fail(THROTTLE_NAME, ip);
        return Err(ApiError::unauthorized("Wrong admin password."));
    }
    db.throttle.lock().unwrap().success(THROTTLE_NAME, ip);
    let token = format!("a_{}", crate::codec::ids::random_b62(43));
    db.admin_sessions.lock().unwrap().insert(sha_hex(&token), crate::engine::now());
    Ok(json!({"token": token, "idle_minutes": db.cfg().admin_session_idle_minutes}))
}

pub fn logout(db: &Db, token: &str) -> Value {
    db.admin_sessions.lock().unwrap().remove(&sha_hex(token));
    json!({"message": "Signed out."})
}

/// True when the token is a live web admin session (refreshes its idle timer).
pub fn check(db: &Db, token: &str) -> bool {
    if !token.starts_with("a_") || !enabled(db) {
        return false;
    }
    let idle = db.cfg().admin_session_idle_minutes.max(1) * 60_000;
    let now = crate::engine::now();
    let mut s = db.admin_sessions.lock().unwrap();
    let h = sha_hex(token);
    match s.get_mut(&h) {
        Some(last) if now.saturating_sub(*last) <= idle => {
            *last = now;
            true
        }
        Some(_) => {
            s.remove(&h);
            false
        }
        None => false,
    }
}

fn settings_json(c: &Config) -> Value {
    json!({
        "port": c.port,
        "bind": c.bind,
        "tls": c.tls,
        "tls_cert": c.tls_cert,
        "tls_key": c.tls_key,
        "session_idle_hours": c.session_idle_hours,
        "notify_keep": c.notify_keep,
        "private_key_ttl": c.private_key_ttl,
        "login_free_attempts": c.login_free_attempts,
        "login_max_delay": c.login_max_delay,
        "max_indexes": c.max_indexes,
        "max_tags": c.max_tags,
        "admin_session_idle_minutes": c.admin_session_idle_minutes,
        "rocksdb.block_cache_mb": c.rocksdb.block_cache_mb,
        "rocksdb.blob_min_kb": c.rocksdb.blob_min_kb,
    })
}

pub fn settings_get(db: &Db) -> ApiResult<Value> {
    let running = db.cfg();
    let file = Config::load(&db.data_dir).map_err(ApiError::internal)?;
    let (r, f) = (settings_json(&running), settings_json(&file));
    let pending: Vec<&str> = ON_RESTART.iter().copied().filter(|k| r[k] != f[k]).collect();
    Ok(json!({"running": r, "saved": f, "live": LIVE, "on_restart": ON_RESTART, "pending_restart": pending,
              "file": Config::path(&db.data_dir).display().to_string()}))
}

fn as_u64(k: &str, v: &Value, min: u64) -> ApiResult<u64> {
    v.as_u64().filter(|n| *n >= min).ok_or_else(|| ApiError::invalid(format!("{k} must be a whole number of at least {min}.")))
}

fn as_dur(k: &str, v: &Value) -> ApiResult<String> {
    let s = v.as_str().ok_or_else(|| ApiError::invalid(format!("{k} must be a duration like 30m, 12h or 1d.")))?;
    match parse_duration_ms(s) {
        Some(ms) if ms > 0 => Ok(s.trim().to_string()),
        _ => Err(ApiError::invalid(format!("{k} = \"{s}\" is not a duration. Use forms like 30m, 12h, 1d."))),
    }
}

/// Saves changes to dadabada.toml; live settings apply now, the rest on the next start.
pub fn settings_set(db: &Db, changes: &Map<String, Value>) -> ApiResult<Value> {
    let mut file = Config::load(&db.data_dir).map_err(ApiError::internal)?;
    for (k, v) in changes {
        match k.as_str() {
            "port" => file.port = u16::try_from(as_u64(k, v, 1)?).map_err(|_| ApiError::invalid("port must be 1 to 65535."))?,
            "bind" => {
                let s = v.as_str().ok_or_else(|| ApiError::invalid("bind must be an address like 0.0.0.0."))?;
                s.parse::<std::net::IpAddr>().map_err(|_| ApiError::invalid(format!("bind = \"{s}\" is not an IP address.")))?;
                file.bind = s.to_string();
            }
            "tls" => file.tls = v.as_bool().ok_or_else(|| ApiError::invalid("tls must be true or false."))?,
            "tls_cert" | "tls_key" => {
                let s = match v {
                    Value::Null => None,
                    Value::String(s) if s.trim().is_empty() => None,
                    Value::String(s) => {
                        if !std::path::Path::new(s).is_file() {
                            return Err(ApiError::invalid(format!("{k}: the file {s} does not exist on the server.")));
                        }
                        Some(s.clone())
                    }
                    _ => return Err(ApiError::invalid(format!("{k} must be a file path."))),
                };
                if k == "tls_cert" {
                    file.tls_cert = s;
                } else {
                    file.tls_key = s;
                }
            }
            "session_idle_hours" => file.session_idle_hours = as_u64(k, v, 1)?,
            "notify_keep" => file.notify_keep = as_dur(k, v)?,
            "private_key_ttl" => file.private_key_ttl = as_dur(k, v)?,
            "login_max_delay" => file.login_max_delay = as_dur(k, v)?,
            "login_free_attempts" => file.login_free_attempts = as_u64(k, v, 1)? as u32,
            "max_indexes" => file.max_indexes = as_u64(k, v, 0)? as usize,
            "max_tags" => file.max_tags = as_u64(k, v, 0)? as usize,
            "admin_session_idle_minutes" => file.admin_session_idle_minutes = as_u64(k, v, 1)?,
            "rocksdb.block_cache_mb" => file.rocksdb.block_cache_mb = as_u64(k, v, 8)?,
            "rocksdb.blob_min_kb" => file.rocksdb.blob_min_kb = as_u64(k, v, 1)?,
            other => return Err(ApiError::invalid(format!("Unknown setting \"{other}\"."))),
        }
    }
    if file.tls {
        let cert = file.tls_cert.clone().map(std::path::PathBuf::from).unwrap_or_else(|| db.data_dir.join("tls/cert.pem"));
        if !cert.is_file() {
            super::tls::make_self_signed(&db.data_dir).map_err(ApiError::internal)?;
            file.tls_cert = Some(db.data_dir.join("tls/cert.pem").display().to_string());
            file.tls_key = Some(db.data_dir.join("tls/key.pem").display().to_string());
        }
    }
    file.save(&db.data_dir).map_err(ApiError::internal)?;
    // live settings take effect now
    let mut run = db.cfg();
    run.session_idle_hours = file.session_idle_hours;
    run.notify_keep = file.notify_keep.clone();
    run.private_key_ttl = file.private_key_ttl.clone();
    run.login_free_attempts = file.login_free_attempts;
    run.login_max_delay = file.login_max_delay.clone();
    run.max_indexes = file.max_indexes;
    run.max_tags = file.max_tags;
    run.admin_session_idle_minutes = file.admin_session_idle_minutes;
    db.set_cfg(run);
    settings_get(db)
}

/// Create a record as a user (records belong to users), or update any record as admin.
pub fn record_post(db: &Db, v: &Value) -> ApiResult<Value> {
    let body = v.get("body").cloned().unwrap_or(json!({}));
    let is_update = body.pointer("/config/record_id").is_some_and(|r| !r.is_null());
    if is_update {
        return write::post_record(db, &Caller::admin(), &body, "admin");
    }
    let who = v
        .get("as_user")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::invalid("Choose the user who will own the new record (as_user)."))?;
    let u = users::find_user(db, who)?;
    let c = Caller { user_id: u.user_id, username: u.username, level: u.level, admin: false, agent: None };
    write::post_record(db, &c, &body, "admin")
}
