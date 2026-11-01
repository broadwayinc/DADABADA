//! The admin socket: `<data>/admin.sock`, mode 0600, same OS user only. One JSON line per request.

use crate::codec::keys::{sort_pos, S_INI, S_INI_REV, S_LOOSE, S_LOOSE_REV};
use crate::codec::loose::LOOSE_VERSION;
use crate::engine::access::{grantees, Caller};
use crate::engine::views::record_keys;
use crate::engine::{agents, catalog, query, transfer, users, write, Db};
use crate::error::{ApiError, ApiResult};
use crate::store::{Batch, CF_IX, CF_META, CF_REC};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

pub fn socket_path(data: &Path) -> PathBuf {
    data.join("admin.sock")
}

pub async fn serve(db: Arc<Db>, path: PathBuf) -> Result<(), String> {
    let _ = std::fs::remove_file(&path);
    let l = UnixListener::bind(&path).map_err(|e| format!("Could not open the admin socket {}: {e}", path.display()))?;
    super::tls::set_mode(&path, 0o600);
    let me = unsafe { libc::getuid() };
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = l.accept().await else { continue };
            if let Ok(cred) = stream.peer_cred() {
                if cred.uid() != me {
                    continue;
                }
            }
            let db = db.clone();
            tokio::spawn(async move {
                let (r, mut w) = stream.into_split();
                let mut lines = BufReader::new(r).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let d2 = db.clone();
                    let res = tokio::task::spawn_blocking(move || {
                        let v: Value = serde_json::from_str(&line).map_err(|e| ApiError::invalid(format!("bad admin request: {e}")))?;
                        admin(&d2, &v)
                    })
                    .await
                    .unwrap_or_else(|e| Err(ApiError::internal(e.to_string())));
                    let out = match res {
                        Ok(v) => json!({"ok": v}),
                        Err(e) => json!({"error": {"code": e.code, "message": e.message}}),
                    };
                    let mut s = out.to_string();
                    s.push('\n');
                    if w.write_all(s.as_bytes()).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    Ok(())
}

fn s<'a>(v: &'a Value, k: &str) -> ApiResult<&'a str> {
    v.get(k).and_then(Value::as_str).ok_or_else(|| ApiError::invalid(format!("\"{k}\" is required.")))
}

fn level_of(v: &Value) -> ApiResult<u8> {
    match v.get("level") {
        None | Some(Value::Null) => Ok(0),
        Some(l) => crate::engine::access::parse_level(l)?
            .parse::<u8>()
            .map_err(|_| ApiError::invalid("A user level is 00 to 99 (** is only for records).")),
    }
}

pub fn admin(db: &Db, v: &Value) -> ApiResult<Value> {
    let cmd = s(v, "cmd")?;
    let a = Caller::admin();
    let body = v.get("body").cloned().unwrap_or(json!({}));
    match cmd {
        "user_add" => {
            let u = users::add_user(db, s(v, "username")?, s(v, "password")?, level_of(v)?)?;
            Ok(u.public_json())
        }
        "user_list" => Ok(json!(users::list_users(db)?.iter().map(|u| u.public_json()).collect::<Vec<_>>())),
        "user_search" => users::search_users(db, &body),
        "users_del" => {
            // batch: user ids or usernames; each account goes now, its records in the background
            let list = v.get("users").and_then(Value::as_array).ok_or_else(|| ApiError::invalid("\"users\" must be a list of user ids or usernames."))?;
            // "*" anywhere in the list means every user
            let list: Vec<String> = if list.iter().any(|u| u.as_str() == Some("*")) {
                users::list_users(db)?.into_iter().map(|u| u.user_id).collect()
            } else {
                list.iter().map(|u| u.as_str().unwrap_or("").to_string()).collect()
            };
            let mut deleted = vec![];
            let mut failed = vec![];
            for who in &list {
                let who = who.as_str();
                match users::delete_user(db, who) {
                    Ok(x) => deleted.push(x.public_json()),
                    Err(e) => failed.push(json!({"user": who, "code": e.code, "message": e.message})),
                }
            }
            Ok(json!({"deleted": deleted, "failed": failed, "outbox": db.outbox_depth()}))
        }
        "records_count" => {
            // how many records a getRecords query matches (every page), for a delete confirmation
            let mut b = body.clone();
            let mut n = 0u64;
            let mut pages = 0;
            loop {
                b["fetch"] = json!({"limit": 1000, "start_key": b["fetch"]["start_key"].clone()});
                let r = query::get_records(db, &a, &b)?;
                n += r["list"].as_array().map(|l| l.len()).unwrap_or(0) as u64;
                pages += 1;
                if r["end_of_list"].as_bool().unwrap_or(true) || pages > 1000 {
                    break;
                }
                b["fetch"]["start_key"] = r["start_key"].clone();
            }
            Ok(json!({"count": n, "capped": pages > 1000}))
        }
        "records_del_query" => {
            // delete everything a getRecords query matches, page after page
            let mut b = body.clone();
            let mut deleted = 0u64;
            let mut failed = vec![];
            loop {
                b["fetch"] = json!({"limit": 1000});
                let r = query::get_records(db, &a, &b)?;
                let ids: Vec<String> = r["list"].as_array().map(|l| l.iter().filter_map(|x| x["record_id"].as_str().map(String::from)).collect()).unwrap_or_default();
                if ids.is_empty() {
                    break;
                }
                for id in ids {
                    match write::delete_record(db, &a, &id, false) {
                        Ok(_) => deleted += 1,
                        Err(e) => {
                            if failed.len() < 20 {
                                failed.push(json!({"record_id": id, "code": e.code, "message": e.message}));
                            }
                        }
                    }
                }
                if r["end_of_list"].as_bool().unwrap_or(true) {
                    break;
                }
            }
            Ok(json!({"deleted": deleted, "failed": failed, "outbox": db.outbox_depth()}))
        }
        "records_del_all" => {
            // every record in the database; users, keys and settings stay
            let mut deleted = 0u64;
            loop {
                let mut ids = vec![];
                db.store.scan_prefix(crate::store::CF_REC, b"", |k, _| {
                    ids.push(String::from_utf8_lossy(k).into_owned());
                    ids.len() < 1000
                })?;
                if ids.is_empty() {
                    break;
                }
                for id in ids {
                    if write::delete_record(db, &a, &id, true).is_ok() {
                        deleted += 1;
                    }
                }
            }
            Ok(json!({"deleted": deleted, "outbox": db.outbox_depth()}))
        }
        "users_reindex" => Ok(json!({"users": users::reindex_users(db)?})),
        "user_info" => Ok(users::find_user(db, s(v, "user")?)?.public_json()),
        "user_level" => Ok(users::set_level(db, s(v, "user")?, level_of(v)?)?.public_json()),
        "user_del" => {
            let u = users::delete_user(db, s(v, "user")?)?;
            Ok(json!({"deleted": u.public_json(), "outbox": db.outbox_depth()}))
        }
        "records" => {
            let mut b = body.clone();
            if let Some(u) = b.get("user").and_then(Value::as_str).map(String::from) {
                b["user_id"] = json!(users::find_user(db, &u)?.user_id);
            }
            query::admin_list(db, &b)
        }
        "record_get" => query::record_json_by_id(db, s(v, "record_id")?),
        "record_del" => {
            let m = write::delete_record(db, &a, s(v, "record_id")?, false)?;
            Ok(json!({"deleted": s(v, "record_id")?, "collection": m.collection}))
        }
        "query" => query::get_records(db, &a, &body),
        "collections" => catalog::get_collections(db, &a, &body),
        "indexes" => catalog::get_indexes(db, &a, &body),
        "tags" => catalog::get_tags(db, &a, &body),
        "uniques" => catalog::get_unique_id(db, &a, &body),
        "status" => status(db),
        "backup" => {
            let dir = s(v, "dir")?;
            if Path::new(dir).exists() {
                return Err(ApiError::exists(format!("{dir} already exists; give a new folder name.")));
            }
            rocksdb::checkpoint::Checkpoint::new(&db.store.db)
                .and_then(|c| c.create_checkpoint(dir))
                .map_err(|e| ApiError::internal(format!("backup failed: {e}")))?;
            let cfg = Path::new(dir).join("dadabada.toml.copy");
            let _ = std::fs::copy(crate::config::Config::path(&db.data_dir), cfg);
            Ok(json!({"backup": dir}))
        }
        "agents" => {
            let user = match v.get("user").and_then(Value::as_str) {
                Some(u) => Some(users::find_user(db, u)?.user_id),
                None => None,
            };
            Ok(json!(agents::list(db, &a, user.as_deref())?.iter().map(|k| k.json()).collect::<Vec<_>>()))
        }
        "agent_revoke" => Ok(agents::revoke(db, &a, s(v, "key_id")?)?.json()),
        "secret_rotate" => {
            let new: [u8; 32] = rand::random();
            db.store.put(CF_META, b"secret", &new)?;
            *db.secret.write().unwrap() = new;
            Ok(json!({"message": "Made a new server secret. Every private access key given out before now no longer works."}))
        }
        "rebuild_loose" => rebuild_loose(db),
        "export" => transfer::export(db, s(v, "file")?, v.get("collection").and_then(Value::as_str), v.get("user").and_then(Value::as_str)),
        "import" => transfer::import(db, s(v, "file")?, v.get("replace").and_then(Value::as_bool).unwrap_or(false)),
        "ping" => Ok(json!("pong")),
        "shutdown" => {
            // stop cleanly (as on Ctrl-C); with restart, start again in the same process afterwards
            let restart = v.get("restart").and_then(Value::as_bool).unwrap_or(false);
            db.restart.store(restart, Ordering::SeqCst);
            db.shutdown.notify_one();
            Ok(json!({
                "message": if restart { "Restarting: the server stops cleanly and starts again in the same process." } else { "Stopping: the server stops cleanly." },
                "pid": std::process::id(),
                "restart": restart,
            }))
        }
        "logs" => {
            // server log lines after `after` (0 = everything kept), oldest first
            let after = v.get("after").and_then(Value::as_u64).unwrap_or(0);
            let (last, lines) = crate::server::log_since(after);
            Ok(json!({"last": last, "kept": crate::server::LOG_KEEP, "list": lines.iter().map(|(seq, line)| json!({"seq": seq, "line": line})).collect::<Vec<_>>()}))
        }
        "web_password" => super::web_admin::set_password(db, s(v, "password")?),
        "web_password_off" => super::web_admin::turn_off(db),
        "web_status" => Ok(json!({"web_admin": super::web_admin::enabled(db), "sessions": db.admin_sessions.lock().unwrap().len()})),
        "settings_get" => super::web_admin::settings_get(db),
        "settings_set" => super::web_admin::settings_set(db, v.get("settings").and_then(Value::as_object).ok_or_else(|| ApiError::invalid("\"settings\" must be an object."))?),
        "record_post" => super::web_admin::record_post(db, v),
        "record_grants" => write::list_grants(db, &a, &json!({"record_id": s(v, "record_id")?})),
        _ => Err(ApiError::invalid(format!("Unknown admin command \"{cmd}\"."))),
    }
}

pub fn stored_loose_version(db: &Db) -> u32 {
    db.store
        .get(CF_META, b"loose_version")
        .ok()
        .flatten()
        .map(|v| u32::from_be_bytes(v[..4].try_into().unwrap_or([0; 4])))
        .unwrap_or(LOOSE_VERSION)
}

fn status(db: &Db) -> ApiResult<Value> {
    let (colls, recs) = catalog::collection_count(db)?;
    Ok(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "data": db.data_dir.display().to_string(),
        "port": db.cfg().port,
        "tls": db.cfg().tls,
        "users": users::list_users(db)?.len(),
        "records": recs,
        "collections": colls,
        "outbox": db.outbox_depth(),
        "sessions": db.sessions.lock().unwrap().len(),
        "connections": db.stats.connections.load(Ordering::Relaxed),
        "requests": db.stats.requests.load(Ordering::Relaxed),
        "listeners": db.listeners.lock().unwrap().len(),
        "max_indexes": db.cfg().max_indexes,
        "max_tags": db.cfg().max_tags,
        "notify_keep": db.cfg().notify_keep,
        "private_key_ttl": db.cfg().private_key_ttl,
        "web_admin": super::web_admin::enabled(db),
        "web_admin_sessions": db.admin_sessions.lock().unwrap().len(),
        "loose_version": stored_loose_version(db),
        "loose_version_built": LOOSE_VERSION,
    }))
}

/// Rewrites every loose and initials key with the current rules.
fn rebuild_loose(db: &Db) -> ApiResult<Value> {
    let loose = |k: &[u8]| sort_pos(k).map(|p| matches!(k.get(p), Some(&S_LOOSE | &S_LOOSE_REV | &S_INI | &S_INI_REV))).unwrap_or(false);
    let mut removed = 0u64;
    let mut dead = vec![];
    db.store.scan_prefix(CF_IX, b"", |k, _| {
        if loose(k) {
            dead.push(k.to_vec());
        }
        true
    })?;
    for chunk in dead.chunks(10_000) {
        let mut b = Batch::new(&db.store);
        for k in chunk {
            b.delete(CF_IX, k);
            removed += 1;
        }
        b.commit()?;
    }
    let mut ids = vec![];
    db.store.scan_prefix(CF_REC, b"", |k, _| {
        ids.push(String::from_utf8_lossy(k).into_owned());
        true
    })?;
    let mut added = 0u64;
    for id in &ids {
        let _g = db.lock_many(&[id]);
        let Some(m) = db.load_meta(id)? else { continue };
        let gs = grantees(db, id)?;
        let mut b = Batch::new(&db.store);
        for k in record_keys(id, &m, &gs) {
            if loose(&k) {
                b.put(CF_IX, &k, b"");
                added += 1;
            }
        }
        b.commit()?;
    }
    db.store.put(CF_META, b"loose_version", &LOOSE_VERSION.to_be_bytes())?;
    Ok(json!({"records": ids.len(), "removed_keys": removed, "written_keys": added, "loose_version": LOOSE_VERSION}))
}
