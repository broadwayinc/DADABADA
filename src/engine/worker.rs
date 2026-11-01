//! Drains the outbox: counters, cascades, feed, notifications, user deletion (PLAN.md section 6).

use super::access::{self, Caller};
use super::catalog::{self, IdxStat};
use super::events::Event;
use super::subs::{self, FeedRow};
use super::users;
use super::{Db, Notice};
use crate::codec::keys::{self, push_comp, record_id_of, refby_prefix, T_BOOL, T_NUM};
use crate::error::ApiResult;
use crate::store::{Batch, CF_ACC, CF_EVT, CF_FEED, CF_IX, CF_META, CF_NTF, CF_SUB};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

const BATCH: usize = 4096;
const PAGE: usize = 200;
/// The outbox is compacted after this many removed events, so its deletion markers do not pile up on disk.
const COMPACT_EVERY: u64 = 500_000;

/// Where the worker reads the outbox from: every event numbered below `pos` has been handled. Reading from
/// here, and not from the start, keeps each pass as fast as the first; a pass from the start would step
/// over the deletion marker of every event handled since the last compaction.
pub struct Cursor {
    pos: Vec<u8>,
    removed: u64,
}

impl Cursor {
    pub fn new() -> Cursor {
        Cursor { pos: Vec::new(), removed: 0 }
    }
}

pub fn spawn(db: Arc<Db>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("dadabada-worker".into())
        .spawn(move || run(db))
        .expect("worker thread")
}

fn run(db: Arc<Db>) {
    let mut last_prune = 0u64;
    let mut cur = Cursor::new();
    loop {
        // when stopping, the batch in hand is finished; the rest stays in the outbox for the next start
        if db.stopping.load(Ordering::SeqCst) {
            break;
        }
        let did = match drain_once(&db, &mut cur) {
            Ok(n) => n,
            Err(e) => {
                crate::server::log(format!("{}  worker: {e}", crate::server::clock()));
                std::thread::sleep(Duration::from_millis(500));
                0
            }
        };
        let now = super::now();
        if now - last_prune > 60_000 {
            last_prune = now;
            if let Err(e) = prune_notifications(&db) {
                crate::server::log(format!("{}  worker: notification cleanup failed: {e}", crate::server::clock()));
            }
        }
        if cur.removed >= COMPACT_EVERY {
            let evt = db.store.cf(CF_EVT);
            db.store.db.compact_range_cf(evt, None::<&[u8]>, Some(cur.pos.as_slice()));
            cur.removed = 0;
        }
        if did > 0 {
            continue;
        }
        let (m, cv) = &db.wake;
        let mut woke = m.lock().unwrap();
        if !*woke {
            woke = cv.wait_timeout(woke, Duration::from_millis(1000)).unwrap().0;
        }
        *woke = false;
    }
}

/// Applies up to BATCH events from the cursor on, and moves the cursor. Returns how many were read.
pub fn drain_once(db: &Db, cur: &mut Cursor) -> ApiResult<usize> {
    // Every event numbered below `settled` is stored. One above it can be stored while a lower one is
    // still on its way, so the cursor may move up to `settled` but never past it.
    let settled = db.store.outbox.settled_below().to_be_bytes().to_vec();
    let mut evs: Vec<(Vec<u8>, Event)> = vec![];
    let mut bad = vec![];
    let mut end = cur.pos.clone();
    db.store.scan(CF_EVT, &cur.pos, &[0xFF; 9], true, |k, v| {
        match rmp_serde::from_slice::<Event>(v) {
            Ok(e) => evs.push((k.to_vec(), e)),
            Err(_) => bad.push(k.to_vec()),
        }
        end = k.to_vec();
        end.push(0); // the smallest key after k
        evs.len() + bad.len() < BATCH
    })?;
    let n = evs.len() + bad.len();
    if n == 0 {
        return Ok(0);
    }
    let mut removed = bad.len() as u64;
    if !bad.is_empty() {
        let mut b = Batch::new(&db.store);
        for k in &bad {
            b.remove_event(k);
        }
        b.commit()?;
    }

    // 1. coalesce counters into one batch
    let mut colls: BTreeMap<String, (i64, i64, BTreeMap<String, i64>)> = BTreeMap::new();
    let mut tags: BTreeMap<(String, String), i64> = BTreeMap::new();
    let mut idxs: BTreeMap<(String, String), IdxStat> = BTreeMap::new();
    let mut user_recs: BTreeMap<String, i64> = BTreeMap::new();
    let mut subc: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    let mut b = Batch::new(&db.store);
    let mut actions = vec![];
    for (k, e) in evs {
        match e {
            Event::Coll { coll, level, d, size } => {
                let c = colls.entry(coll).or_default();
                c.0 += d;
                c.1 += size;
                *c.2.entry(level).or_default() += d;
                b.remove_event(&k);
                removed += 1;
            }
            Event::Tag { coll, tag, d } => {
                *tags.entry((coll, tag)).or_default() += d;
                b.remove_event(&k);
                removed += 1;
            }
            Event::Idx { coll, name, kind, num, d } => {
                let s = idxs.entry((coll, name)).or_default();
                s.records += d;
                match kind {
                    T_NUM => {
                        s.num_count += d;
                        s.num_total += num * d as f64;
                    }
                    T_BOOL => {
                        s.bool_count += d;
                        s.bool_total += num as i64 * d;
                    }
                    _ => s.str_count += d,
                }
                b.remove_event(&k);
                removed += 1;
            }
            Event::UserRecords { user, d } => {
                *user_recs.entry(user).or_default() += d;
                b.remove_event(&k);
                removed += 1;
            }
            Event::SubCount { user, subscribers, subscribed } => {
                let s = subc.entry(user).or_default();
                s.0 += subscribers;
                s.1 += subscribed;
                b.remove_event(&k);
                removed += 1;
            }
            Event::Uniq { id, record_id, d } => {
                catalog::apply_uniq(&mut b, &id, &record_id, d);
                b.remove_event(&k);
                removed += 1;
            }
            other => actions.push((k, other)),
        }
    }
    for (c, (d, size, lv)) in &colls {
        catalog::apply_coll(db, &mut b, c, *d, *size, lv)?;
    }
    for ((c, t), d) in &tags {
        if *d != 0 {
            catalog::apply_tag(db, &mut b, c, t, *d)?;
        }
    }
    for ((c, name), s) in &idxs {
        catalog::apply_idx(db, &mut b, c, name, s)?;
    }
    b.commit()?;
    let keys: std::collections::BTreeSet<&String> = user_recs.keys().chain(subc.keys()).collect();
    for u in keys {
        let r = user_recs.get(u).copied().unwrap_or(0);
        let (s1, s2) = subc.get(u).copied().unwrap_or((0, 0));
        if r != 0 || s1 != 0 || s2 != 0 {
            users::bump_user(db, u, r, s1, s2)?;
        }
    }

    // 2. actions, each with its own batch that also removes the event. An event that stays (an error, a
    // deleted user with more records to remove, or a stop) keeps the cursor at it, so it is read again.
    let mut kept: Option<Vec<u8>> = None;
    for (k, e) in actions {
        if db.stopping.load(Ordering::SeqCst) {
            kept.get_or_insert(k);
            break;
        }
        match act(db, &k, &e) {
            Ok(true) => removed += 1,
            Ok(false) => {
                kept.get_or_insert(k);
            }
            Err(err) => {
                crate::server::log(format!("{}  worker: {err} (event kept, retrying)", crate::server::clock()));
                std::thread::sleep(Duration::from_millis(200));
                kept.get_or_insert(k);
                break;
            }
        }
    }
    let mut next = end.min(settled);
    if let Some(k) = kept {
        next = next.min(k);
    }
    cur.pos = cur.pos.clone().max(next);
    cur.removed += removed;
    Ok(n)
}

fn done(db: &Db, k: &[u8]) -> ApiResult<bool> {
    let mut b = Batch::new(&db.store);
    b.remove_event(k);
    b.commit()?;
    Ok(true)
}

/// Carries out one event. Returns whether the event is finished (and removed).
fn act(db: &Db, k: &[u8], e: &Event) -> ApiResult<bool> {
    match e {
        Event::RefCount { record_id } => {
            super::write::sync_refcount_keys(db, record_id)?;
            done(db, k)
        }
        Event::Removed { record_id, meta, cascade } => {
            if *cascade {
                cascade_removed(db, record_id, meta.source.can_remove_referencing_records)?;
            }
            done(db, k)
        }
        Event::Feed { record_id, uploader, level: _, to } => {
            let targets = match to {
                Some(t) => vec![t.clone()],
                None => subs::active_subscribers(db, uploader, true)?,
            };
            let row = serde_json::to_vec(&FeedRow { uploader: uploader.clone(), direct: to.is_some() }).unwrap();
            let mut b = Batch::new(&db.store);
            for t in targets {
                b.put(CF_FEED, &subs::feed_key(&t, record_id), &row);
            }
            b.remove_event(k);
            b.commit()?;
            Ok(true)
        }
        Event::Notify { record_id, uploader, collection, title, body, to } => {
            let targets = match to {
                Some(t) => vec![t.clone()],
                None => subs::active_subscribers(db, uploader, false)?,
            };
            // only users who can read the record hear about it (its level, grants, subscription rules)
            let meta = db.load_meta(record_id)?;
            let mut readers = vec![];
            for t in targets {
                let Some(m) = &meta else { break };
                let Some(u) = super::users::get_user(db, &t)? else { continue };
                let tc = access::Caller { user_id: u.user_id, username: u.username, level: u.level, admin: false, agent: None };
                if access::can_read(db, &tc, record_id, m, None)? {
                    readers.push(t);
                }
            }
            let targets = readers;
            let mut b = Batch::new(&db.store);
            let mut live = vec![];
            for t in targets {
                let seq = db.next_ntf_seq();
                let data = json!({
                    "from": uploader, "record_id": record_id, "collection": collection,
                    "title": title, "body": body, "created": super::now(),
                });
                b.put(CF_NTF, &ntf_key(&t, seq), &serde_json::to_vec(&data).unwrap());
                live.push((t, Notice { seq, data }));
            }
            b.remove_event(k);
            b.commit()?;
            let ls = db.listeners.lock().unwrap();
            for (t, n) in live {
                if let Some(tx) = ls.get(&t) {
                    let _ = tx.send(n);
                }
            }
            Ok(true)
        }
        Event::UserDeleted { user_id } => remove_user_page(db, k, user_id),
        _ => done(db, k),
    }
}

pub fn ntf_key(user: &str, seq: u64) -> Vec<u8> {
    let mut k = Vec::new();
    push_comp(&mut k, user);
    k.extend_from_slice(&seq.to_be_bytes());
    k
}

/// Records referencing a deleted record: delete them (can_remove_referencing_records) or drop the reference.
fn cascade_removed(db: &Db, target: &str, remove: bool) -> ApiResult<()> {
    let p = refby_prefix(target);
    loop {
        let mut ids = vec![];
        db.store.scan_prefix(CF_IX, &p, |k, _| {
            ids.push(record_id_of(k).to_string());
            ids.len() < PAGE
        })?;
        if ids.is_empty() {
            return Ok(());
        }
        // either action removes the record's refby key, so the loop ends
        for id in ids {
            if remove {
                if super::write::delete_record(db, &Caller::admin(), &id, true).is_err() {
                    super::write::clear_reference(db, &id)?;
                }
            } else {
                super::write::clear_reference(db, &id)?;
            }
        }
    }
}

/// One page of a deleted user's leftovers. Returns false while more pages remain (the event stays).
fn remove_user_page(db: &Db, k: &[u8], user: &str) -> ApiResult<bool> {
    let mut ids = vec![];
    db.store.scan_prefix(CF_IX, &keys::owner_prefix(user), |key, _| {
        ids.push(record_id_of(key).to_string());
        ids.len() < PAGE
    })?;
    if !ids.is_empty() {
        for id in ids {
            let _ = super::write::delete_record(db, &Caller::admin(), &id, true);
        }
        return Ok(false); // event stays; next pass continues
    }
    // grants given to the user
    let up = access::acc_user_prefix(user);
    let mut granted = vec![];
    db.store.scan_prefix(CF_ACC, &up, |key, _| {
        granted.push(String::from_utf8_lossy(&key[up.len()..]).into_owned());
        true
    })?;
    for rid in granted {
        let _g = db.lock_many(&[&rid]);
        let mut b = Batch::new(&db.store);
        if let Some(m) = db.load_meta(&rid)? {
            for key in super::views::grantee_keys(&rid, &m, user) {
                b.delete(CF_IX, &key);
            }
        }
        b.delete(CF_ACC, &access::acc_rec_key(&rid, user));
        b.delete(CF_ACC, &access::acc_user_key(user, &rid));
        b.commit()?;
    }
    // subscriptions both ways
    let mut b = Batch::new(&db.store);
    for (tag, other_is_target) in [(b's', true), (b't', false)] {
        let mut p = vec![tag];
        push_comp(&mut p, user);
        let mut others = vec![];
        db.store.scan_prefix(CF_SUB, &p, |key, _| {
            others.push(String::from_utf8_lossy(&key[p.len()..]).into_owned());
            true
        })?;
        for o in others {
            if other_is_target {
                b.delete(CF_SUB, &access::sub_key(&o, user));
                b.delete(CF_SUB, &access::sub_rkey(user, &o));
                db.push_event(&mut b, &Event::SubCount { user: o, subscribers: -1, subscribed: 0 });
            } else {
                b.delete(CF_SUB, &access::sub_key(user, &o));
                b.delete(CF_SUB, &access::sub_rkey(&o, user));
                db.push_event(&mut b, &Event::SubCount { user: o, subscribers: 0, subscribed: -1 });
            }
        }
    }
    let mut bp = vec![b'b'];
    push_comp(&mut bp, user);
    b.delete_range(CF_SUB, &bp, &keys::prefix_end(&bp));
    let fp = subs::feed_prefix(user);
    b.delete_range(CF_FEED, &fp, &keys::prefix_end(&fp));
    let np = {
        let mut p = Vec::new();
        push_comp(&mut p, user);
        p
    };
    b.delete_range(CF_NTF, &np, &keys::prefix_end(&np));
    b.remove_event(k);
    b.commit()?;
    super::agents::delete_user_keys(db, user)?;
    db.listeners.lock().unwrap().remove(user);
    Ok(true)
}

/// Deletes notifications older than the keep time and records the highest pruned id per user.
/// A user's notifications are stored in the order they were made, so only the expired ones and the first
/// one still kept are read for each user; the rest of the user's notifications are skipped with one seek.
pub fn prune_notifications(db: &Db) -> ApiResult<()> {
    let cutoff = super::now().saturating_sub(db.cfg().notify_keep_ms());
    let store = &db.store;
    let mut b = Batch::new(store);
    let mut pending = 0usize;
    let mut it = store.db.raw_iterator_cf(store.cf(CF_NTF));
    it.seek_to_first();
    while it.valid() {
        let k = it.key().unwrap();
        if k.len() < 9 {
            it.next();
            continue;
        }
        // a key is the user, a separator and an 8-byte sequence number
        let user_p = k[..k.len() - 8].to_vec();
        let mut last_dead: Option<u64> = None;
        while it.valid() {
            let k = it.key().unwrap();
            if k.len() != user_p.len() + 8 || !k.starts_with(&user_p) {
                break;
            }
            let created = serde_json::from_slice::<serde_json::Value>(it.value().unwrap())
                .ok()
                .and_then(|j| j["created"].as_u64())
                .unwrap_or(0);
            if created >= cutoff {
                break;
            }
            last_dead = Some(u64::from_be_bytes(k[k.len() - 8..].try_into().unwrap()));
            b.delete(CF_NTF, k);
            pending += 1;
            it.next();
        }
        if let Some(seq) = last_dead {
            let mut mk = b"w".to_vec();
            mk.extend_from_slice(&user_p[..user_p.len() - 1]);
            b.put(CF_META, &mk, &seq.to_be_bytes());
        }
        if pending >= 10_000 {
            std::mem::replace(&mut b, Batch::new(store)).commit()?;
            pending = 0;
        }
        it.seek(keys::prefix_end(&user_p));
    }
    it.status()?;
    b.commit()
}
