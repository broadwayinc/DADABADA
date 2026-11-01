//! Subscriptions, blocking, feed.

use super::access::{can_read, is_active_subscriber, sub_key, sub_rkey, Caller};
use super::events::Event;
use super::json::*;
use super::users::get_user;
use super::Db;
use crate::codec::keys::push_comp;
use crate::error::{ApiError, ApiResult};
use crate::store::{Batch, CF_FEED, CF_SUB};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SubRow {
    #[serde(default)]
    pub get_feed: bool,
    #[serde(default)]
    pub get_notified: bool,
    #[serde(default)]
    pub blocked: bool,
    #[serde(default)]
    pub timestamp: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FeedRow {
    pub uploader: String,
    /// put there by a reference to the subscriber's own record
    #[serde(default)]
    pub direct: bool,
}

pub fn feed_key(subscriber: &str, record_id: &str) -> Vec<u8> {
    let mut k = Vec::new();
    push_comp(&mut k, subscriber);
    k.extend_from_slice(record_id.as_bytes());
    k
}

pub fn feed_prefix(subscriber: &str) -> Vec<u8> {
    let mut k = Vec::new();
    push_comp(&mut k, subscriber);
    k
}

fn row_json(target: &str, subscriber: &str, r: &SubRow) -> Value {
    json!({
        "subscription": target,
        "subscriber": subscriber,
        "get_feed": r.get_feed,
        "get_notified": r.get_notified,
        "blocked": r.blocked,
        "timestamp": r.timestamp,
    })
}

/// Kept when a blocked subscriber unsubscribes, so subscribing again comes back blocked.
/// 'b' keys never fall inside the 't' / 's' subscription scans.
pub fn block_key(target: &str, subscriber: &str) -> Vec<u8> {
    let mut k = vec![b'b'];
    push_comp(&mut k, target);
    k.extend_from_slice(subscriber.as_bytes());
    k
}

fn get_row(db: &Db, target: &str, subscriber: &str) -> ApiResult<Option<SubRow>> {
    Ok(db.store.get(CF_SUB, &sub_key(target, subscriber))?.and_then(|v| serde_json::from_slice(&v).ok()))
}

fn put_row(b: &mut Batch, target: &str, subscriber: &str, r: &SubRow) {
    let v = serde_json::to_vec(r).unwrap();
    b.put(CF_SUB, &sub_key(target, subscriber), &v);
    b.put(CF_SUB, &sub_rkey(subscriber, target), &v);
}

fn other_user(db: &Db, c: &Caller, o: &serde_json::Map<String, Value>) -> ApiResult<String> {
    c.check_write()?;
    let u = req_str(o, "user_id")?.to_string();
    if u == c.user_id {
        return Err(ApiError::invalid("user_id cannot be your own."));
    }
    if get_user(db, &u)?.is_none() {
        return Err(ApiError::not_found(format!("User {u} does not exist.")));
    }
    Ok(u)
}

pub fn subscribe(db: &Db, c: &Caller, body: &Value) -> ApiResult<Value> {
    let o = obj(body)?;
    let target = other_user(db, c, o)?;
    let _g = db.lock_many(&[&format!("sub:{target}:{}", c.user_id)]);
    let existing = get_row(db, &target, &c.user_id)?;
    let blocked_before = existing.is_none() && db.store.exists(CF_SUB, &block_key(&target, &c.user_id))?;
    let mut r = existing.clone().unwrap_or(SubRow { timestamp: super::now(), blocked: blocked_before, ..Default::default() });
    if let Some(v) = opt_bool(o, "get_feed")? {
        r.get_feed = v;
    }
    if let Some(v) = opt_bool(o, "get_notified")? {
        r.get_notified = v;
    }
    let mut b = Batch::new(&db.store);
    put_row(&mut b, &target, &c.user_id, &r);
    if existing.is_none() {
        db.push_event(&mut b, &Event::SubCount { user: target.clone(), subscribers: 1, subscribed: 0 });
        db.push_event(&mut b, &Event::SubCount { user: c.user_id.clone(), subscribers: 0, subscribed: 1 });
    }
    b.commit()?;
    db.wake_worker();
    Ok(row_json(&target, &c.user_id, &r))
}

pub fn unsubscribe(db: &Db, c: &Caller, body: &Value) -> ApiResult<Value> {
    let o = obj(body)?;
    let target = other_user(db, c, o)?;
    let _g = db.lock_many(&[&format!("sub:{target}:{}", c.user_id)]);
    let row = get_row(db, &target, &c.user_id)?.ok_or_else(|| ApiError::not_found(format!("You do not subscribe to {target}.")))?;
    let mut b = Batch::new(&db.store);
    if row.blocked {
        b.put(CF_SUB, &block_key(&target, &c.user_id), b"");
    }
    b.delete(CF_SUB, &sub_key(&target, &c.user_id));
    b.delete(CF_SUB, &sub_rkey(&c.user_id, &target));
    db.push_event(&mut b, &Event::SubCount { user: target.clone(), subscribers: -1, subscribed: 0 });
    db.push_event(&mut b, &Event::SubCount { user: c.user_id.clone(), subscribers: 0, subscribed: -1 });
    b.commit()?;
    db.wake_worker();
    Ok(json!({"message": format!("Unsubscribed from {target}.")}))
}

pub fn block(db: &Db, c: &Caller, body: &Value, blocked: bool) -> ApiResult<Value> {
    let o = obj(body)?;
    let subscriber = other_user(db, c, o)?;
    let _g = db.lock_many(&[&format!("sub:{}:{subscriber}", c.user_id)]);
    let mk = block_key(&c.user_id, &subscriber);
    let row = get_row(db, &c.user_id, &subscriber)?;
    // unblocking someone who left while blocked: only the marker is there
    if row.is_none() && !blocked && db.store.exists(CF_SUB, &mk)? {
        let mut b = Batch::new(&db.store);
        b.delete(CF_SUB, &mk);
        b.commit()?;
        return Ok(json!({"subscription": c.user_id, "subscriber": subscriber, "blocked": false}));
    }
    let mut r = row.ok_or_else(|| ApiError::not_found(format!("User {subscriber} does not subscribe to you.")))?;
    r.blocked = blocked;
    let mut b = Batch::new(&db.store);
    if !blocked {
        b.delete(CF_SUB, &mk);
    }
    put_row(&mut b, &c.user_id, &subscriber, &r);
    b.commit()?;
    Ok(row_json(&c.user_id, &subscriber, &r))
}

pub fn get_subscriptions(db: &Db, c: &Caller, body: &Value) -> ApiResult<Value> {
    let o = obj(body)?;
    let subscriber = opt_str(o, "subscriber")?;
    let subscription = opt_str(o, "subscription")?;
    let blocked = opt_bool(o, "blocked")?;
    let f = fetch_opts(body, true)?;
    let (prefix, mine) = match (subscriber, subscription) {
        (Some(s), Some(t)) => {
            let row = get_row(db, t, s)?;
            if !c.admin && s != c.user_id && t != c.user_id {
                return Err(ApiError::denied("You can only list your own subscriptions and subscribers."));
            }
            let list: Vec<Value> = row.iter().filter(|r| blocked.is_none_or(|b| r.blocked == b)).map(|r| row_json(t, s, r)).collect();
            return Ok(page(list, None, true));
        }
        (Some(s), None) => {
            let mut p = b"s".to_vec();
            push_comp(&mut p, s);
            (p, s == c.user_id)
        }
        (None, Some(t)) => {
            let mut p = b"t".to_vec();
            push_comp(&mut p, t);
            (p, t == c.user_id)
        }
        _ => return Err(ApiError::invalid("Give subscriber, subscription, or both.")),
    };
    if !c.admin && !mine {
        return Err(ApiError::denied("You can only list your own subscriptions and subscribers."));
    }
    let mut lo = prefix.clone();
    if let Some(sk) = &f.start_key {
        lo = sk.clone();
        lo.push(0);
    }
    let hi = crate::codec::keys::prefix_end(&prefix);
    let mut list = vec![];
    let mut last = None;
    let mut end = true;
    db.store.scan(CF_SUB, &lo, &hi, true, |k, v| {
        let other = String::from_utf8_lossy(&k[prefix.len()..]).into_owned();
        let r: SubRow = serde_json::from_slice(v).unwrap_or_default();
        if blocked.is_some_and(|b| r.blocked != b) {
            return true;
        }
        if list.len() >= f.limit {
            end = false;
            return false;
        }
        let j = if prefix[0] == b's' { row_json(&other, subscriber.unwrap(), &r) } else { row_json(subscription.unwrap(), &other, &r) };
        list.push(j);
        last = Some(k.to_vec());
        true
    })?;
    Ok(page(list, last, end))
}

/// Subscribers of `target` that want feed / notifications and are not blocked.
pub fn active_subscribers(db: &Db, target: &str, feed: bool) -> ApiResult<Vec<String>> {
    let mut p = b"t".to_vec();
    push_comp(&mut p, target);
    let mut out = vec![];
    db.store.scan_prefix(CF_SUB, &p, |k, v| {
        let r: SubRow = serde_json::from_slice(v).unwrap_or_default();
        if !r.blocked && ((feed && r.get_feed) || (!feed && r.get_notified)) {
            out.push(String::from_utf8_lossy(&k[p.len()..]).into_owned());
        }
        true
    })?;
    Ok(out)
}

pub fn get_feed(db: &Db, c: &Caller, body: &Value) -> ApiResult<Value> {
    let f = fetch_opts(body, false)?;
    let q = &query_of(body, false)?;
    let max_level = match q.get("access_level").filter(|v| !v.is_null()) {
        Some(v) => {
            // the highest level to include: "00" to "99" ("**" is not a level here; readable private records always show)
            let l = super::access::parse_level(v)?;
            l.parse::<u8>().map_err(|_| ApiError::invalid("query.access_level for getFeed is the highest level to include: \"00\" to \"99\"."))?.min(c.eff_level())
        }
        None => c.eff_level(),
    };
    let prefix = feed_prefix(&c.user_id);
    let (mut lo, mut hi) = (prefix.clone(), crate::codec::keys::prefix_end(&prefix));
    if let Some(sk) = &f.start_key {
        if f.ascending {
            lo = sk.clone();
            lo.push(0);
        } else {
            hi = sk.clone();
        }
    }
    let mut rows = vec![];
    let mut list = vec![];
    let mut last = None;
    let mut end = true;
    let mut stale = vec![];
    db.store.scan(CF_FEED, &lo, &hi, f.ascending, |k, v| {
        rows.push((k.to_vec(), serde_json::from_slice::<FeedRow>(v).unwrap_or_default()));
        rows.len() < f.limit * 4 + 64
    })?;
    let full_scan = rows.len() < f.limit * 4 + 64;
    for (k, row) in rows {
        let id = crate::codec::keys::record_id_of(&k).to_string();
        let Some((m, data)) = db.load_record(&id)? else {
            stale.push(k);
            continue;
        };
        if !row.direct && !is_active_subscriber(db, &row.uploader, &c.user_id)? {
            continue;
        }
        if m.level_num().map(|l| l > max_level).unwrap_or(true) && m.user_id != c.user_id {
            if !(m.is_private() && can_read(db, c, &id, &m, None)?) {
                continue;
            }
        }
        if !can_read(db, c, &id, &m, None)? {
            continue;
        }
        if list.len() >= f.limit {
            end = false;
            break;
        }
        list.push(super::json::record_json(&id, &m, &data));
        last = Some(k);
    }
    if !full_scan && end {
        end = false;
    }
    if !stale.is_empty() {
        let mut b = Batch::new(&db.store);
        for k in stale {
            b.delete(CF_FEED, &k);
        }
        b.commit()?;
    }
    Ok(page(list, last, end))
}
