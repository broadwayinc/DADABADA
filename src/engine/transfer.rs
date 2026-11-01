//! export / import as JSON lines (ARCHITECTURE.md, "Commands, settings and export").

use super::access;
use super::agents::{self, AgentKey};
use super::events::Event;
use super::subs::SubRow;
use super::users::{self, User};
use super::write::{import_record, Imported};
use super::Db;
use crate::codec::body;
use crate::error::{ApiError, ApiResult};
use crate::store::{Batch, CF_ACC, CF_IX, CF_REC, CF_SUB};
use rocksdb::ReadOptions;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::io::{BufRead, BufWriter, Write};

fn record_line(id: &str, m: &body::Meta, data: &[u8]) -> Value {
    let data: Value = serde_json::from_slice(data).unwrap_or(Value::Null);
    json!({
        "type": "record",
        "record_id": id,
        "user_id": m.user_id,
        "agent": m.agent,
        "uploaded": m.uploaded,
        "updated": m.updated,
        "ip": m.ip,
        "data": data,
        "config": {
            "collection": {"name": m.collection, "access_level": m.level, "subscription": m.sub},
            "index": m.index.iter().map(|(n, v)| json!({"name": n, "value": v.to_json()})).collect::<Vec<_>>(),
            "tags": m.tags,
            "reference": m.reference,
            "unique_id": m.unique_id,
            "source": m.source,
            "readonly": m.readonly,
        }
    })
}

pub fn export(db: &Db, path: &str, coll: Option<&str>, user: Option<&str>) -> ApiResult<Value> {
    let user_id = match user {
        Some(u) => Some(users::find_user(db, u)?.user_id),
        None => None,
    };
    let filtered = coll.is_some() || user_id.is_some();
    let f = std::fs::File::create(path).map_err(|e| ApiError::invalid(format!("Could not create {path}: {e}")))?;
    let mut w = BufWriter::new(f);
    let line = |w: &mut BufWriter<std::fs::File>, v: &Value| -> ApiResult<()> {
        serde_json::to_writer(&mut *w, v).map_err(|e| ApiError::internal(e.to_string()))?;
        w.write_all(b"\n")?;
        Ok(())
    };
    let snap = db.store.db.snapshot();
    let mut records = vec![];
    let mut uploaders = BTreeSet::new();
    {
        let mut ro = ReadOptions::default();
        ro.set_snapshot(&snap);
        let mut it = db.store.db.raw_iterator_cf_opt(db.store.cf(CF_REC), ro);
        it.seek_to_first();
        while it.valid() {
            let id = String::from_utf8_lossy(it.key().unwrap()).into_owned();
            if let Some((m, d)) = body::decode(it.value().unwrap()) {
                let keep = coll.is_none_or(|c| m.collection == c) && user_id.as_deref().is_none_or(|u| m.user_id == u);
                if keep {
                    uploaders.insert(m.user_id.clone());
                    records.push(record_line(&id, &m, d));
                }
            }
            it.next();
        }
        it.status()?;
    }
    let mut n_users = 0;
    for u in users::list_users(db)? {
        if filtered && !uploaders.contains(&u.user_id) {
            continue;
        }
        let mut j = serde_json::to_value(&u).unwrap();
        j["type"] = json!("user");
        line(&mut w, &j)?;
        n_users += 1;
    }
    let mut n_grants = 0;
    let ids: BTreeSet<String> = records.iter().map(|r| r["record_id"].as_str().unwrap_or("").to_string()).collect();
    for r in &records {
        line(&mut w, r)?;
    }
    for id in &ids {
        for g in access::grantees(db, id)? {
            line(&mut w, &json!({"type": "grant", "record_id": id, "user_id": g}))?;
            n_grants += 1;
        }
    }
    let mut n_subs = 0;
    if !filtered {
        let mut rows = vec![];
        db.store.scan_prefix(CF_SUB, b"t", |k, v| {
            let rest = &k[1..];
            if let Some(p) = rest.iter().position(|b| *b == 0) {
                rows.push((
                    String::from_utf8_lossy(&rest[..p]).into_owned(),
                    String::from_utf8_lossy(&rest[p + 1..]).into_owned(),
                    serde_json::from_slice::<SubRow>(v).unwrap_or_default(),
                ));
            }
            true
        })?;
        for (t, s, r) in rows {
            line(&mut w, &json!({"type": "subscription", "subscription": t, "subscriber": s, "row": r}))?;
            n_subs += 1;
        }
    }
    let mut n_keys = 0;
    for (h, a) in agents::export_rows(db)? {
        if filtered && !uploaders.contains(&a.user_id) {
            continue;
        }
        line(&mut w, &json!({"type": "agent_key", "hash": h, "key": a}))?;
        n_keys += 1;
    }
    w.flush()?;
    Ok(json!({
        "file": path, "users": n_users, "records": records.len(), "grants": n_grants,
        "subscriptions": n_subs, "agent_keys": n_keys,
    }))
}

pub fn import(db: &Db, path: &str, replace: bool) -> ApiResult<Value> {
    let f = std::fs::File::open(path).map_err(|e| ApiError::invalid(format!("Could not open {path}: {e}")))?;
    let (mut nu, mut nr, mut ng, mut ns, mut nk, mut skipped) = (0, 0, 0, 0, 0, 0);
    let mut failed: Vec<Value> = vec![];
    for (i, l) in std::io::BufReader::new(f).lines().enumerate() {
        let l = l?;
        if l.trim().is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(&l) {
            Ok(v) => v,
            Err(e) => {
                failed.push(json!({"line": i + 1, "message": format!("not JSON: {e}")}));
                continue;
            }
        };
        let r: ApiResult<bool> = (|| match v["type"].as_str().unwrap_or("") {
            "user" => {
                let u: User = serde_json::from_value(v.clone()).map_err(|e| ApiError::invalid(e.to_string()))?;
                if users::get_user(db, &u.user_id)?.is_some() && !replace {
                    return Ok(false);
                }
                if let Some(o) = users::get_user_by_name(db, &u.username)? {
                    if o.user_id != u.user_id {
                        return Err(ApiError::exists(format!("username {} belongs to another user here", u.username)));
                    }
                }
                let mut b = Batch::new(&db.store);
                let old = users::get_user(db, &u.user_id)?;
                users::put_user_from(&mut b, old.as_ref(), &User { records: 0, subscribers: 0, subscribed: 0, ..u });
                b.commit()?;
                nu += 1;
                Ok(true)
            }
            "record" => {
                let id = v["record_id"].as_str().unwrap_or("").to_string();
                let uid = v["user_id"].as_str().unwrap_or("").to_string();
                if users::get_user(db, &uid)?.is_none() {
                    return Err(ApiError::not_found(format!("record {id}: user {uid} is not here")));
                }
                if db.store.exists(CF_REC, id.as_bytes())? {
                    if !replace {
                        return Ok(false);
                    }
                    super::write::delete_record(db, &access::Caller::admin(), &id, true)?;
                }
                import_record(
                    db,
                    &json!({"data": v["data"], "config": v["config"]}),
                    Imported {
                        record_id: id,
                        user_id: uid,
                        agent: serde_json::from_value(v["agent"].clone()).unwrap_or(None),
                        uploaded: v["uploaded"].as_u64().unwrap_or(0),
                        updated: v["updated"].as_u64().unwrap_or(0),
                        ip: v["ip"].as_str().unwrap_or("").to_string(),
                    },
                )?;
                nr += 1;
                Ok(true)
            }
            "grant" => {
                let id = v["record_id"].as_str().unwrap_or("");
                let u = v["user_id"].as_str().unwrap_or("");
                let _g = db.lock_many(&[id]);
                let m = db.load_meta(id)?.ok_or_else(|| ApiError::not_found(format!("grant: record {id} is not here")))?;
                let mut b = Batch::new(&db.store);
                b.put(CF_ACC, &access::acc_rec_key(id, u), json!({"granted_by": m.user_id, "at": super::now()}).to_string().as_bytes());
                b.put(CF_ACC, &access::acc_user_key(u, id), b"");
                for k in super::views::grantee_keys(id, &m, u) {
                    b.put(CF_IX, &k, b"");
                }
                b.commit()?;
                ng += 1;
                Ok(true)
            }
            "subscription" => {
                let t = v["subscription"].as_str().unwrap_or("");
                let s = v["subscriber"].as_str().unwrap_or("");
                let row: SubRow = serde_json::from_value(v["row"].clone()).unwrap_or_default();
                let new = !db.store.exists(CF_SUB, &access::sub_key(t, s))?;
                let mut b = Batch::new(&db.store);
                let rv = serde_json::to_vec(&row).unwrap();
                b.put(CF_SUB, &access::sub_key(t, s), &rv);
                b.put(CF_SUB, &access::sub_rkey(s, t), &rv);
                if new {
                    db.push_event(&mut b, &Event::SubCount { user: t.to_string(), subscribers: 1, subscribed: 0 });
                    db.push_event(&mut b, &Event::SubCount { user: s.to_string(), subscribers: 0, subscribed: 1 });
                }
                b.commit()?;
                ns += 1;
                Ok(true)
            }
            "agent_key" => {
                let a: AgentKey = serde_json::from_value(v["key"].clone()).map_err(|e| ApiError::invalid(e.to_string()))?;
                agents::import_row(db, v["hash"].as_str().unwrap_or(""), &a)?;
                nk += 1;
                Ok(true)
            }
            other => Err(ApiError::invalid(format!("unknown line type \"{other}\""))),
        })();
        match r {
            Ok(true) => {}
            Ok(false) => skipped += 1,
            Err(e) => failed.push(json!({"line": i + 1, "message": e.message})),
        }
    }
    db.wake_worker();
    let nfail = failed.len();
    failed.truncate(20);
    Ok(json!({
        "users": nu, "records": nr, "grants": ng, "subscriptions": ns, "agent_keys": nk,
        "skipped": skipped, "failed_count": nfail, "failed": failed,
    }))
}
