//! Who can read and write what. See ARCHITECTURE.md, "Access rules".

use super::agents::AgentKey;
use super::Db;
use crate::codec::body::Meta;
use crate::codec::keys::push_comp;
use crate::error::{ApiError, ApiResult};
use crate::store::{CF_ACC, CF_SUB};
use hmac::{Hmac, Mac};
use sha2::Sha256;

/// The identity behind a request.
#[derive(Debug, Clone)]
pub struct Caller {
    pub user_id: String,
    pub username: String,
    pub level: u8,
    /// The OS user on the admin socket.
    pub admin: bool,
    pub agent: Option<AgentKey>,
}

impl Caller {
    pub fn admin() -> Caller {
        Caller { user_id: String::new(), username: "admin".into(), level: 99, admin: true, agent: None }
    }

    pub fn eff_level(&self) -> u8 {
        if self.admin {
            return 99;
        }
        match &self.agent {
            Some(a) => self.level.min(a.max_level.unwrap_or(99)),
            None => self.level,
        }
    }

    pub fn private_ok(&self) -> bool {
        self.admin || self.agent.as_ref().map(|a| a.private).unwrap_or(true)
    }

    pub fn coll_ok(&self, coll: &str) -> bool {
        match &self.agent {
            Some(a) if !a.collections.is_empty() => a.collections.iter().any(|c| c == coll),
            _ => true,
        }
    }

    pub fn check_coll(&self, coll: &str) -> ApiResult<()> {
        if self.coll_ok(coll) {
            Ok(())
        } else {
            Err(ApiError::denied(format!("This agent key is not allowed to use the collection \"{coll}\".")))
        }
    }

    pub fn check_write(&self) -> ApiResult<()> {
        if let Some(a) = &self.agent {
            if a.read_only {
                return Err(ApiError::denied("This agent key is read-only."));
            }
        }
        Ok(())
    }

    pub fn who(&self) -> String {
        match &self.agent {
            Some(a) => format!("{}/{}", self.username, a.name),
            None => self.username.clone(),
        }
    }
}

pub fn acc_rec_key(record_id: &str, user: &str) -> Vec<u8> {
    let mut k = vec![b'r'];
    push_comp(&mut k, record_id);
    k.extend_from_slice(user.as_bytes());
    k
}

pub fn acc_user_key(user: &str, record_id: &str) -> Vec<u8> {
    let mut k = vec![b'u'];
    push_comp(&mut k, user);
    k.extend_from_slice(record_id.as_bytes());
    k
}

pub fn acc_rec_prefix(record_id: &str) -> Vec<u8> {
    let mut k = vec![b'r'];
    push_comp(&mut k, record_id);
    k
}

pub fn acc_user_prefix(user: &str) -> Vec<u8> {
    let mut k = vec![b'u'];
    push_comp(&mut k, user);
    k
}

/// Users granted access to a record.
pub fn grantees(db: &Db, record_id: &str) -> ApiResult<Vec<String>> {
    let p = acc_rec_prefix(record_id);
    let mut out = vec![];
    db.store.scan_prefix(CF_ACC, &p, |k, _| {
        out.push(String::from_utf8_lossy(&k[p.len()..]).into_owned());
        true
    })?;
    Ok(out)
}

pub fn is_granted(db: &Db, record_id: &str, user: &str) -> ApiResult<bool> {
    db.store.exists(CF_ACC, &acc_rec_key(record_id, user))
}

/// Subscription row key: `t target \0 subscriber` and `s subscriber \0 target`.
pub fn sub_key(target: &str, subscriber: &str) -> Vec<u8> {
    let mut k = vec![b't'];
    push_comp(&mut k, target);
    k.extend_from_slice(subscriber.as_bytes());
    k
}

pub fn sub_rkey(subscriber: &str, target: &str) -> Vec<u8> {
    let mut k = vec![b's'];
    push_comp(&mut k, subscriber);
    k.extend_from_slice(target.as_bytes());
    k
}

/// True when `subscriber` follows `target` and is not blocked.
pub fn is_active_subscriber(db: &Db, target: &str, subscriber: &str) -> ApiResult<bool> {
    match db.store.get(CF_SUB, &sub_key(target, subscriber))? {
        Some(v) => {
            let s: super::subs::SubRow = serde_json::from_slice(&v).unwrap_or_default();
            Ok(!s.blocked)
        }
        None => Ok(false),
    }
}

/// Can this caller read this record?
pub fn can_read(db: &Db, c: &Caller, id: &str, m: &Meta, private_key: Option<&str>) -> ApiResult<bool> {
    if c.admin {
        return Ok(true);
    }
    if !c.coll_ok(&m.collection) {
        return Ok(false);
    }
    let own = m.user_id == c.user_id;
    if m.is_private() && !c.private_ok() {
        return Ok(false);
    }
    if !own && m.sub.is_subscription_record && !is_active_subscriber(db, &m.user_id, &c.user_id)? {
        return Ok(false);
    }
    if own {
        return Ok(true);
    }
    if let Some(l) = m.level_num() {
        if l <= c.eff_level() {
            return Ok(true);
        }
    }
    // private, or above the caller's level
    if is_granted(db, id, &c.user_id)? {
        return Ok(true);
    }
    if let Some(k) = private_key {
        if check_private_key(db, k, id, &c.user_id) {
            return Ok(true);
        }
    }
    reach_by_reference(db, c, m)
}

/// A record is reachable when the caller uploaded, or was granted, the record it references.
pub fn reach_by_reference(db: &Db, c: &Caller, m: &Meta) -> ApiResult<bool> {
    if let Some(t) = &m.reference {
        if let Some(tm) = db.load_meta(t)? {
            if tm.user_id == c.user_id || is_granted(db, t, &c.user_id)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

type HmacSha = Hmac<Sha256>;

fn key_mac(db: &Db, record_id: &str, user: &str, expiry: u64) -> Vec<u8> {
    let mut mac = <HmacSha as Mac>::new_from_slice(&db.secret()).expect("hmac key");
    mac.update(record_id.as_bytes());
    mac.update(&[0]);
    mac.update(user.as_bytes());
    mac.update(&[0]);
    mac.update(&expiry.to_be_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// `hex(expiry) + hex(hmac(secret, record_id \0 user \0 expiry))`
pub fn make_private_key(db: &Db, record_id: &str, user: &str) -> (String, u64) {
    let expiry = super::now() + db.cfg().private_key_ttl_ms();
    (format!("{}{}", hex::encode(expiry.to_be_bytes()), hex::encode(key_mac(db, record_id, user, expiry))), expiry)
}

pub fn check_private_key(db: &Db, key: &str, record_id: &str, user: &str) -> bool {
    if key.len() != 16 + 64 {
        return false;
    }
    let Ok(exp) = hex::decode(&key[..16]) else { return false };
    let expiry = u64::from_be_bytes(exp.try_into().unwrap());
    if expiry < super::now() {
        return false;
    }
    let Ok(mac) = hex::decode(&key[16..]) else { return false };
    let mut m = <HmacSha as Mac>::new_from_slice(&db.secret()).expect("hmac key");
    m.update(record_id.as_bytes());
    m.update(&[0]);
    m.update(user.as_bytes());
    m.update(&[0]);
    m.update(&expiry.to_be_bytes());
    m.verify_slice(&mac).is_ok()
}

/// Access levels: "00".."99" (numbers 0..99 accepted) or "**".
pub fn parse_level(v: &serde_json::Value) -> ApiResult<String> {
    let bad = || ApiError::invalid("access_level must be \"00\" to \"99\", or \"**\" for private.");
    match v.as_str() {
        Some(s) if s == "**" || (s.len() == 2 && s.bytes().all(|b| b.is_ascii_digit())) => Ok(s.to_string()),
        _ => Err(bad()),
    }
}
