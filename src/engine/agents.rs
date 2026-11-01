//! Agent keys: revocable keys with a level cap, read-only, private and collection limits.

use super::access::Caller;
use super::json::*;
use super::users::sha_hex;
use super::Db;
use crate::codec::ids;
use crate::codec::keys::push_comp;
use crate::codec::text::clean_name;
use crate::config::parse_duration_ms;
use crate::error::{ApiError, ApiResult};
use crate::store::{Batch, CF_KEY};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentKey {
    pub key_id: String,
    pub user_id: String,
    pub name: String,
    pub max_level: Option<u8>,
    pub read_only: bool,
    pub private: bool,
    pub collections: Vec<String>,
    pub expires: Option<u64>,
    pub created: u64,
}

impl AgentKey {
    pub fn json(&self) -> Value {
        json!({
            "key_id": self.key_id,
            "user_id": self.user_id,
            "name": self.name,
            "max_level": self.max_level.map(|l| format!("{l:02}")),
            "read_only": self.read_only,
            "private": self.private,
            "collections": self.collections,
            "expires": self.expires,
            "created": self.created,
        })
    }
}

fn hkey(hash: &str) -> Vec<u8> {
    let mut k = b"h".to_vec();
    k.extend_from_slice(hash.as_bytes());
    k
}
fn ikey(key_id: &str) -> Vec<u8> {
    let mut k = b"k".to_vec();
    k.extend_from_slice(key_id.as_bytes());
    k
}
fn user_prefix(user: &str) -> Vec<u8> {
    let mut k = b"u".to_vec();
    push_comp(&mut k, user);
    k
}

pub fn create(db: &Db, c: &Caller, body: &Value) -> ApiResult<Value> {
    if c.admin || c.agent.is_some() {
        return Err(ApiError::denied("Agent keys are made by a user signed in with a password."));
    }
    let o = obj(body)?;
    let name = clean_name("agent key name", req_str(o, "name")?)?;
    let max_level = match o.get("max_level") {
        None | Some(Value::Null) => None,
        Some(v) => {
            let bad = || ApiError::invalid("max_level must be \"00\" to \"99\".");
            Some(super::access::parse_level(v).map_err(|_| bad())?.parse::<u8>().map_err(|_| bad())?)
        }
    };
    let collections = match o.get("collections") {
        None | Some(Value::Null) => vec![],
        Some(v) => str_list(v, "collection")?.iter().map(|c| clean_name("collection name", c)).collect::<ApiResult<_>>()?,
    };
    let expires = match o.get("expires") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(super::now() + parse_duration_ms(s).ok_or_else(|| ApiError::invalid("expires must be like 30m, 12h, 30d, or a time in ms."))?),
        Some(v) => Some(v.as_u64().ok_or_else(|| ApiError::invalid("expires must be like 30d or a time in ms."))?),
    };
    let key = format!("dk_{}", ids::random_b62(43));
    let mut key_id = ids::new_key_id();
    while db.store.exists(CF_KEY, &ikey(&key_id))? {
        key_id = ids::new_key_id();
    }
    let a = AgentKey {
        key_id: key_id.clone(),
        user_id: c.user_id.clone(),
        name,
        max_level,
        read_only: opt_bool(o, "read_only")?.unwrap_or(false),
        private: opt_bool(o, "private")?.unwrap_or(false),
        collections,
        expires,
        created: super::now(),
    };
    let h = sha_hex(&key);
    let mut b = Batch::new(&db.store);
    b.put(CF_KEY, &hkey(&h), &serde_json::to_vec(&a).unwrap());
    b.put(CF_KEY, &ikey(&key_id), h.as_bytes());
    let mut uk = user_prefix(&c.user_id);
    uk.extend_from_slice(key_id.as_bytes());
    b.put(CF_KEY, &uk, h.as_bytes());
    b.commit()?;
    let mut j = a.json();
    j["key"] = json!(key);
    Ok(j)
}

pub fn resolve(db: &Db, key: &str) -> ApiResult<Option<AgentKey>> {
    let Some(v) = db.store.get(CF_KEY, &hkey(&sha_hex(key)))? else { return Ok(None) };
    let a: AgentKey = serde_json::from_slice(&v).map_err(|e| ApiError::internal(e.to_string()))?;
    if let Some(e) = a.expires {
        if e < super::now() {
            return Ok(None);
        }
    }
    Ok(Some(a))
}

fn load_by_id(db: &Db, key_id: &str) -> ApiResult<Option<(String, AgentKey)>> {
    let Some(h) = db.store.get(CF_KEY, &ikey(key_id))? else { return Ok(None) };
    let h = String::from_utf8_lossy(&h).into_owned();
    Ok(db.store.get(CF_KEY, &hkey(&h))?.and_then(|v| serde_json::from_slice(&v).ok()).map(|a| (h, a)))
}

pub fn list(db: &Db, c: &Caller, user: Option<&str>) -> ApiResult<Vec<AgentKey>> {
    if c.agent.is_some() {
        return Err(ApiError::denied("An agent key cannot list agent keys."));
    }
    let mut ids = vec![];
    let prefix = match (c.admin, user) {
        (true, None) => b"k".to_vec(),
        (true, Some(u)) => user_prefix(u),
        (false, _) => user_prefix(&c.user_id),
    };
    db.store.scan_prefix(CF_KEY, &prefix, |k, _| {
        let id = &k[prefix.len()..];
        ids.push(String::from_utf8_lossy(id).into_owned());
        true
    })?;
    let mut out = vec![];
    for id in ids {
        if let Some((_, a)) = load_by_id(db, &id)? {
            out.push(a);
        }
    }
    Ok(out)
}

pub fn revoke(db: &Db, c: &Caller, key_id: &str) -> ApiResult<AgentKey> {
    if c.agent.is_some() {
        return Err(ApiError::denied("An agent key cannot revoke agent keys."));
    }
    let (h, a) = load_by_id(db, key_id)?.ok_or_else(|| ApiError::not_found(format!("There is no agent key {key_id}.")))?;
    if !c.admin && a.user_id != c.user_id {
        return Err(ApiError::denied("You can only revoke your own agent keys."));
    }
    let mut b = Batch::new(&db.store);
    b.delete(CF_KEY, &hkey(&h));
    b.delete(CF_KEY, &ikey(key_id));
    let mut uk = user_prefix(&a.user_id);
    uk.extend_from_slice(key_id.as_bytes());
    b.delete(CF_KEY, &uk);
    b.commit()?;
    Ok(a)
}

pub fn delete_user_keys(db: &Db, user: &str) -> ApiResult<()> {
    for a in list(db, &Caller::admin(), Some(user))? {
        revoke(db, &Caller::admin(), &a.key_id)?;
    }
    Ok(())
}

pub fn export_rows(db: &Db) -> ApiResult<Vec<(String, AgentKey)>> {
    let mut out = vec![];
    db.store.scan_prefix(CF_KEY, b"h", |k, v| {
        if let Ok(a) = serde_json::from_slice::<AgentKey>(v) {
            out.push((String::from_utf8_lossy(&k[1..]).into_owned(), a));
        }
        true
    })?;
    Ok(out)
}

pub fn import_row(db: &Db, hash: &str, a: &AgentKey) -> ApiResult<()> {
    let mut b = Batch::new(&db.store);
    b.put(CF_KEY, &hkey(hash), &serde_json::to_vec(a).unwrap());
    b.put(CF_KEY, &ikey(&a.key_id), hash.as_bytes());
    let mut uk = user_prefix(&a.user_id);
    uk.extend_from_slice(a.key_id.as_bytes());
    b.put(CF_KEY, &uk, hash.as_bytes());
    b.commit()
}
