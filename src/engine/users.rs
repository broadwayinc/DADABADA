//! Users, passwords, sessions, failed-login slowdown (PLAN.md section 7).

use super::access::Caller;
use super::events::Event;
use super::Db;
use crate::codec::ids;
use crate::codec::text::clean_name;
use crate::error::{ApiError, ApiResult};
use crate::store::{Batch, CF_USR};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct User {
    pub user_id: String,
    pub username: String,
    pub level: u8,
    pub pw_hash: String,
    pub enc_salt: String,
    pub created: u64,
    #[serde(default)]
    pub records: i64,
    #[serde(default)]
    pub subscribers: i64,
    #[serde(default)]
    pub subscribed: i64,
}

impl User {
    pub fn public_json(&self) -> Value {
        json!({
            "user_id": self.user_id,
            "username": self.username,
            "access_level": format!("{:02}", self.level),
            "records": self.records,
            "subscribers": self.subscribers,
            "subscribed": self.subscribed,
            "created": self.created,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub user_id: String,
    pub created: u64,
    pub last: u64,
    #[serde(skip)]
    pub persisted: u64,
}

#[derive(Default)]
pub struct Throttle {
    by_name: HashMap<String, (u32, u64)>,
    by_ip: HashMap<String, (u32, u64)>,
}

/// KDF settings the client should use with enc_salt.
pub const KDF: (u32, u32, u32) = (65536, 3, 1);

fn ukey(id: &str) -> Vec<u8> {
    let mut k = b"u".to_vec();
    k.extend_from_slice(id.as_bytes());
    k
}
fn nkey(name: &str) -> Vec<u8> {
    let mut k = b"n".to_vec();
    k.extend_from_slice(name.as_bytes());
    k
}
fn skey(hash: &str) -> Vec<u8> {
    let mut k = b"s".to_vec();
    k.extend_from_slice(hash.as_bytes());
    k
}

pub fn sha_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

pub fn get_user(db: &Db, id: &str) -> ApiResult<Option<User>> {
    Ok(db.store.get(CF_USR, &ukey(id))?.and_then(|v| serde_json::from_slice(&v).ok()))
}

pub fn get_user_by_name(db: &Db, name: &str) -> ApiResult<Option<User>> {
    let name = crate::codec::text::nfc(name);
    match db.store.get(CF_USR, &nkey(&name))? {
        Some(id) => get_user(db, &String::from_utf8_lossy(&id)),
        None => Ok(None),
    }
}

/// A username or a user id.
pub fn find_user(db: &Db, s: &str) -> ApiResult<User> {
    if let Some(u) = get_user_by_name(db, s)? {
        return Ok(u);
    }
    get_user(db, s)?.ok_or_else(|| ApiError::not_found(format!("There is no user \"{s}\".")))
}

/// Sorted keys for user search: `x<field>\0<8-byte value>\0<user_id>`, one per field.
pub const USER_FIELDS: [&str; 5] = ["created", "level", "records", "subscribers", "subscribed"];

fn field_value(u: &User, f: &str) -> i64 {
    match f {
        "created" => u.created as i64,
        "level" => u.level as i64,
        "records" => u.records,
        "subscribers" => u.subscribers,
        "subscribed" => u.subscribed,
        _ => 0,
    }
}

fn xprefix(f: &str) -> Vec<u8> {
    let mut k = b"x".to_vec();
    k.extend_from_slice(f.as_bytes());
    k.push(0);
    k
}

fn xkey(f: &str, v: i64, user_id: &str) -> Vec<u8> {
    let mut k = xprefix(f);
    k.extend_from_slice(&crate::codec::keys::enc_num(v as f64));
    k.push(0);
    k.extend_from_slice(user_id.as_bytes());
    k
}

fn user_keys(u: &User) -> Vec<Vec<u8>> {
    USER_FIELDS.iter().map(|f| xkey(f, field_value(u, f), &u.user_id)).collect()
}

/// Writes a user row and keeps its search keys in step with the previous row.
pub fn put_user(b: &mut Batch, u: &User) {
    put_user_from(b, None, u);
}

pub fn put_user_from(b: &mut Batch, old: Option<&User>, u: &User) {
    b.put(CF_USR, &ukey(&u.user_id), &serde_json::to_vec(u).unwrap());
    b.put(CF_USR, &nkey(&u.username), u.user_id.as_bytes());
    let new = user_keys(u);
    if let Some(o) = old {
        for k in user_keys(o) {
            if !new.contains(&k) {
                b.delete(CF_USR, &k);
            }
        }
    }
    for k in new {
        b.put(CF_USR, &k, b"");
    }
}

fn remove_user_keys(b: &mut Batch, u: &User) {
    for k in user_keys(u) {
        b.delete(CF_USR, &k);
    }
}

/// Paged user search: by username or user id (exact or starts with), or by one sortable field with a condition.
pub fn search_users(db: &Db, body: &serde_json::Value) -> ApiResult<serde_json::Value> {
    use super::json::{fetch_opts, obj, opt_str, page};
    let q = &super::json::query_of(body, false)?;
    let o = obj(q)?;
    let f = fetch_opts(body, false)?;
    // username / user id: a lookup, or a prefix listing by username
    if let Some(name) = opt_str(o, "username")? {
        let cond = match opt_str(o, "condition")? { None => "=", Some(c) => super::write::norm_cond(c, false)? };
        let name = crate::codec::text::nfc(name);
        if cond == "=" {
            let list: Vec<_> = get_user_by_name(db, &name)?.into_iter().map(|u| u.public_json()).collect();
            return Ok(page(list, None, true));
        }
        if cond != ">=" {
            return Err(ApiError::invalid("username supports = (exact) or >= (starts with)."));
        }
        let mut p = b"n".to_vec();
        p.extend_from_slice(name.as_bytes());
        let mut ids = vec![];
        db.store.scan_prefix(CF_USR, &p, |_, v| { ids.push(String::from_utf8_lossy(v).into_owned()); true })?;
        let mut list = vec![];
        for id in ids { if let Some(u) = get_user(db, &id)? { list.push(u.public_json()); } }
        return Ok(page(list, None, true));
    }
    if let Some(id) = opt_str(o, "user_id")? {
        let list: Vec<_> = get_user(db, id)?.into_iter().map(|u| u.public_json()).collect();
        return Ok(page(list, None, true));
    }
    let field = opt_str(o, "field")?.unwrap_or("created");
    if !USER_FIELDS.contains(&field) {
        return Err(ApiError::invalid(format!("field must be one of {}.", USER_FIELDS.join(", "))));
    }
    let base = xprefix(field);
    let num = |v: &serde_json::Value| -> ApiResult<i64> {
        if field == "level" { return Ok(super::access::parse_level(v)?.parse::<i64>().map_err(|_| ApiError::invalid("level must be 00 to 99 here."))?); }
        v.as_f64().map(|n| n as i64).ok_or_else(|| ApiError::invalid(format!("{field} takes a number.")))
    };
    let enc = |n: i64| { let mut k = base.clone(); k.extend_from_slice(&crate::codec::keys::enc_num(n as f64)); k };
    let (lo, hi) = match o.get("value").filter(|v| !v.is_null()) {
        None => (base.clone(), crate::codec::keys::prefix_end(&base)),
        Some(v) => {
            let e = enc(num(v)?);
            let after = |mut k: Vec<u8>| { k.push(0xFF); k };
            match (opt_str(o, "condition")?.map(|c| super::write::norm_cond(c, false)).transpose()?.unwrap_or("="), o.get("range").filter(|r| !r.is_null())) {
                (_, Some(r)) => (e, after(enc(num(r)?))),
                ("=", None) => (e.clone(), after(e)),
                (">", None) => (after(e), crate::codec::keys::prefix_end(&base)),
                ("<", None) => (base.clone(), e),
                (">=", None) => (e, crate::codec::keys::prefix_end(&base)),
                _ => (base.clone(), after(e)),
            }
        }
    };
    let (mut lo, mut hi) = (lo, hi);
    if let Some(c) = &f.start_key {
        if f.ascending { let mut a = c.clone(); a.push(0); if a > lo { lo = a; } } else if c < &hi { hi = c.clone(); }
    }
    let mut keys = vec![];
    let mut end = true;
    if lo < hi {
        db.store.scan(CF_USR, &lo, &hi, f.ascending, |k, _| {
            if keys.len() >= f.limit { end = false; return false; }
            keys.push(k.to_vec());
            true
        })?;
    }
    let mut list = vec![];
    for k in &keys {
        let id = String::from_utf8_lossy(&k[k.len() - 8..]).into_owned();
        if let Some(u) = get_user(db, &id)? { list.push(u.public_json()); }
    }
    Ok(page(list, keys.last().cloned(), end))
}

/// Adds to a user's counters (worker).
pub fn bump_user(db: &Db, user_id: &str, records: i64, subscribers: i64, subscribed: i64) -> ApiResult<()> {
    let _g = db.lock_many(&[&format!("user:{user_id}")]);
    if let Some(old) = get_user(db, user_id)? {
        let mut u = old.clone();
        u.records += records;
        u.subscribers += subscribers;
        u.subscribed += subscribed;
        let mut b = Batch::new(&db.store);
        put_user_from(&mut b, Some(&old), &u);
        b.commit()?;
    }
    Ok(())
}

pub fn hash_password(pw: &str) -> ApiResult<String> {
    let salt = SaltString::generate(&mut argon2::password_hash::rand_core::OsRng);
    Argon2::default()
        .hash_password(pw.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| ApiError::internal(format!("password hashing failed: {e}")))
}

pub fn add_user(db: &Db, username: &str, password: &str, level: u8) -> ApiResult<User> {
    let username = clean_name("username", username)?;
    if username.chars().any(char::is_whitespace) {
        return Err(ApiError::invalid("A username cannot contain spaces."));
    }
    if password.is_empty() {
        return Err(ApiError::invalid("The password is empty."));
    }
    if level > 99 {
        return Err(ApiError::invalid("The level must be 00 to 99."));
    }
    let _g = db.lock_many(&[&format!("name:{username}")]);
    if get_user_by_name(db, &username)?.is_some() {
        return Err(ApiError::exists(format!("The username \"{username}\" is already taken.")));
    }
    let mut user_id = ids::new_user_id();
    while get_user(db, &user_id)?.is_some() {
        user_id = ids::new_user_id();
    }
    let u = User {
        user_id,
        username,
        level,
        pw_hash: hash_password(password)?,
        enc_salt: hex::encode(rand::random::<[u8; 16]>()),
        created: super::now(),
        ..Default::default()
    };
    let mut b = Batch::new(&db.store);
    put_user(&mut b, &u);
    b.commit()?;
    Ok(u)
}

/// Rebuilds the search keys of every user (for databases made before user search existed).
pub fn reindex_users(db: &Db) -> ApiResult<usize> {
    let users = list_users(db)?;
    let mut b = Batch::new(&db.store);
    let p = b"x".to_vec();
    b.delete_range(CF_USR, &p, &crate::codec::keys::prefix_end(&p));
    for u in &users {
        for k in user_keys(u) {
            b.put(CF_USR, &k, b"");
        }
    }
    b.commit()?;
    Ok(users.len())
}

pub fn list_users(db: &Db) -> ApiResult<Vec<User>> {
    let mut out = vec![];
    db.store.scan_prefix(CF_USR, b"u", |_, v| {
        if let Ok(u) = serde_json::from_slice::<User>(v) {
            out.push(u);
        }
        true
    })?;
    out.sort_by(|a, b| a.username.cmp(&b.username));
    Ok(out)
}

pub fn set_level(db: &Db, who: &str, level: u8) -> ApiResult<User> {
    if level > 99 {
        return Err(ApiError::invalid("The level must be 00 to 99."));
    }
    let u0 = find_user(db, who)?;
    let _g = db.lock_many(&[&format!("user:{}", u0.user_id)]);
    let old = find_user(db, who)?;
    let mut u = old.clone();
    u.level = level;
    let mut b = Batch::new(&db.store);
    put_user_from(&mut b, Some(&old), &u);
    b.commit()?;
    Ok(u)
}

/// Removes the account now; the worker removes records, grants, subscriptions, keys and feed.
pub fn delete_user(db: &Db, who: &str) -> ApiResult<User> {
    let u = find_user(db, who)?;
    let mut b = Batch::new(&db.store);
    b.delete(CF_USR, &ukey(&u.user_id));
    b.delete(CF_USR, &nkey(&u.username));
    remove_user_keys(&mut b, &u);
    let mut drop = vec![];
    {
        let mut s = db.sessions.lock().unwrap();
        s.retain(|h, sess| {
            if sess.user_id == u.user_id {
                drop.push(h.clone());
                false
            } else {
                true
            }
        });
    }
    for h in drop {
        b.delete(CF_USR, &skey(&h));
    }
    db.push_event(&mut b, &Event::UserDeleted { user_id: u.user_id.clone() });
    b.commit()?;
    db.wake_worker();
    Ok(u)
}

pub fn load_sessions(db: &Db) -> ApiResult<()> {
    let mut map = db.sessions.lock().unwrap();
    db.store.scan_prefix(CF_USR, b"s", |k, v| {
        if let Ok(mut s) = serde_json::from_slice::<Session>(v) {
            s.persisted = s.last;
            map.insert(String::from_utf8_lossy(&k[1..]).into_owned(), s);
        }
        true
    })?;
    Ok(())
}

fn idle_ms(db: &Db) -> u64 {
    db.cfg().session_idle_hours.max(1) * 3_600_000
}

/// Bearer token -> user id, refreshing the idle timer.
pub fn session_user(db: &Db, token: &str) -> ApiResult<Option<String>> {
    let h = sha_hex(token);
    let now = super::now();
    let mut persist = None;
    let user = {
        let mut map = db.sessions.lock().unwrap();
        match map.get_mut(&h) {
            Some(s) if now.saturating_sub(s.last) <= idle_ms(db) => {
                s.last = now;
                if now - s.persisted > 3_600_000 {
                    s.persisted = now;
                    persist = Some(s.clone());
                }
                Some(s.user_id.clone())
            }
            Some(_) => {
                map.remove(&h);
                let _ = db.store.delete(CF_USR, &skey(&h));
                None
            }
            None => None,
        }
    };
    if let Some(s) = persist {
        db.store.put(CF_USR, &skey(&h), &serde_json::to_vec(&s).unwrap())?;
    }
    Ok(user)
}

impl Throttle {
    fn delay(fails: u32, free: u32, max: u64) -> u64 {
        if fails < free {
            return 0;
        }
        let exp = (fails - free).min(30);
        (1000u64 << exp).min(max)
    }

    /// Milliseconds to wait before another try, if any.
    pub fn wait(&self, name: &str, ip: &str, free: u32, max: u64) -> Option<u64> {
        let now = super::now();
        let mut w = 0;
        for (map, k) in [(&self.by_name, name), (&self.by_ip, ip)] {
            if let Some((f, at)) = map.get(k) {
                let until = at + Self::delay(*f, free, max);
                if until > now {
                    w = w.max(until - now);
                }
            }
        }
        (w > 0).then_some(w)
    }

    pub fn fail(&mut self, name: &str, ip: &str) {
        let now = super::now();
        for (map, k) in [(&mut self.by_name, name), (&mut self.by_ip, ip)] {
            let e = map.entry(k.to_string()).or_insert((0, now));
            e.0 += 1;
            e.1 = now;
        }
    }

    pub fn success(&mut self, name: &str, ip: &str) {
        self.by_name.remove(name);
        self.by_ip.remove(ip);
    }
}

/// Before the password check: refuse while a slowdown is running.
pub fn login_gate(db: &Db, username: &str, ip: &str) -> ApiResult<()> {
    let t = db.throttle.lock().unwrap();
    if let Some(ms) = t.wait(username, ip, db.cfg().login_free_attempts, db.cfg().login_max_delay_ms()) {
        let secs = ms.div_ceil(1000);
        let unit = if secs == 1 { "second" } else { "seconds" };
        return Err(ApiError::new(
            429,
            "TOO_MANY_ATTEMPTS",
            format!("Too many failed sign-ins for this username or address. Try again in {secs} {unit}."),
        ));
    }
    Ok(())
}

pub fn login(db: &Db, username: &str, password: &str, ip: &str) -> ApiResult<Value> {
    login_gate(db, username, ip)?;
    let user = get_user_by_name(db, username)?;
    let ok = match &user {
        Some(u) => PasswordHash::new(&u.pw_hash)
            .map(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
            .unwrap_or(false),
        None => {
            // spend the same time so unknown names do not answer faster
            let _ = hash_password(password);
            false
        }
    };
    if !ok {
        db.throttle.lock().unwrap().fail(username, ip);
        return Err(ApiError::unauthorized("Wrong username or password."));
    }
    db.throttle.lock().unwrap().success(username, ip);
    let u = user.unwrap();
    let token = format!("s_{}", ids::random_b62(43));
    let now = super::now();
    let s = Session { user_id: u.user_id.clone(), created: now, last: now, persisted: now };
    let h = sha_hex(&token);
    db.store.put(CF_USR, &skey(&h), &serde_json::to_vec(&s).unwrap())?;
    db.sessions.lock().unwrap().insert(h, s);
    Ok(json!({"token": token, "user": u.public_json()}))
}

pub fn logout(db: &Db, token: &str) -> ApiResult<Value> {
    let h = sha_hex(token);
    db.sessions.lock().unwrap().remove(&h);
    db.store.delete(CF_USR, &skey(&h))?;
    Ok(json!({"message": "Signed out."}))
}

pub fn whoami(db: &Db, c: &Caller) -> ApiResult<Value> {
    let u = get_user(db, &c.user_id)?.ok_or_else(|| ApiError::unauthorized("This account no longer exists."))?;
    let mut j = u.public_json();
    j["enc_salt"] = json!(u.enc_salt);
    j["kdf"] = json!({"alg": "argon2id", "m": KDF.0, "t": KDF.1, "p": KDF.2});
    j["effective_access_level"] = json!(format!("{:02}", c.eff_level()));
    if let Some(a) = &c.agent {
        j["agent"] = a.json();
    }
    Ok(j)
}
