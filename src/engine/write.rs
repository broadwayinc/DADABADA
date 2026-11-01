//! postRecord, update, delete, grants. Every change is one atomic batch plus outbox events.

use super::access::{self, can_read, grantees, is_granted, parse_level, Caller};
use super::events::Event;
use super::json::*;
use super::views::{grantee_keys, record_keys};
use super::Db;
use crate::codec::body::{self, AgentTag, Meta, Notification, Restriction, Source, SubCfg};
use crate::codec::ids;
use crate::codec::keys::*;
use crate::codec::text::{clean_name, clean_unique_id};
use crate::error::{ApiError, ApiResult};
use crate::store::{Batch, CF_ACC, CF_IX, CF_REC, CF_UNQ};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::MutexGuard;

/// How many more ids a new record may draw when the one it drew is taken (see create).
const MAX_ID_TRIES: u32 = 8;

/// The most new records postRecords keeps waiting for one write (see Bulk).
const BULK_MAX: usize = 1000;

/// After another request was seen writing, how long postRecords keeps saving one record at a time before
/// it groups records again. Without it, a steady trickle of single writes from other requests would find a
/// fresh group being saved each time they come, and wait for it.
const BULK_CALM: std::time::Duration = std::time::Duration::from_millis(100);

/// Parsed `config`; `Some(None)` means "set to nothing" (JSON null), `None` means "not sent".
#[derive(Default)]
struct Edit {
    record_id: Option<String>,
    collection: Option<String>,
    level: Option<String>,
    sub: Option<SubCfg>,
    index: Option<Option<Vec<(String, Val)>>>,
    tags: Option<Option<Vec<String>>>,
    reference: Option<Option<String>>,
    unique_id: Option<Option<String>>,
    source: Option<Map<String, Value>>,
    source_reset: bool,
    readonly: Option<bool>,
    notification: Option<Notification>,
    data: Option<Vec<u8>>,
}

/// What a record keeps about the agent key that wrote it.
fn agent_tag(a: &super::agents::AgentKey) -> AgentTag {
    AgentTag::Key { key_id: a.key_id.clone(), name: a.name.clone() }
}

/// For import: keep the original identity and times.
pub struct Imported {
    pub record_id: String,
    pub user_id: String,
    pub agent: Option<AgentTag>,
    pub uploaded: u64,
    pub updated: u64,
    pub ip: String,
}

/// A key named record_id holds a record id: the record, if it exists.
pub fn record_by_id(db: &Db, s: &str) -> ApiResult<Option<String>> {
    Ok((ids::is_record_id(s) && db.store.exists(CF_REC, s.as_bytes())?).then(|| s.to_string()))
}

/// A key named unique_id holds a unique id: the record it names.
pub fn record_by_unique_id(db: &Db, s: &str) -> ApiResult<Option<String>> {
    let u = crate::codec::text::nfc(s);
    Ok(db.store.get(CF_UNQ, u.as_bytes())?.map(|v| String::from_utf8_lossy(&v).into_owned()))
}

/// A plain `reference` string: a record id or a unique id.
pub fn resolve_record_id(db: &Db, s: &str) -> ApiResult<Option<String>> {
    if ids::is_record_id(s) && db.store.exists(CF_REC, s.as_bytes())? {
        return Ok(Some(s.to_string()));
    }
    let u = crate::codec::text::nfc(s);
    Ok(db.store.get(CF_UNQ, u.as_bytes())?.map(|v| String::from_utf8_lossy(&v).into_owned()))
}

fn parse_index(db: &Db, v: &Value) -> ApiResult<Option<Vec<(String, Val)>>> {
    let items: Vec<&Value> = match v {
        Value::Null => return Ok(None),
        Value::Array(a) => a.iter().collect(),
        _ => return Err(ApiError::invalid("index must be a list of {name, value}, or null.")),
    };
    let mut out: Vec<(String, Val)> = vec![];
    let mut seen = BTreeSet::new();
    for it in items {
        let o = it.as_object().ok_or_else(|| ApiError::invalid("Every index entry must be {\"name\": ..., \"value\": ...}."))?;
        let name = clean_name("index name", req_str(o, "name")?)?;
        if name.starts_with('$') {
            return Err(ApiError::invalid(format!("Index name \"{name}\" starts with $; names starting with $ are built in.")));
        }
        if name.ends_with('.') {
            return Err(ApiError::invalid(format!("Index name \"{name}\" ends with a dot; a trailing dot is only for searching children.")));
        }
        let val = to_val(o.get("value").unwrap_or(&Value::Null), &format!("The value of index \"{name}\""))?;
        if !seen.insert(name.clone()) {
            return Err(ApiError::invalid(format!("Index name \"{name}\" appears twice; each name can be used once per record.")));
        }
        out.push((name, val));
    }
    let max = db.cfg().max_indexes;
    if max > 0 && out.len() > max {
        return Err(ApiError::bad("INDEX_LIMIT", format!("This record has {} indexes; this server allows {max} per record (max_indexes).", out.len())));
    }
    Ok(Some(out))
}

fn parse_tags(db: &Db, v: &Value) -> ApiResult<Option<Vec<String>>> {
    let raw: Vec<String> = match v {
        Value::Null => return Ok(None),
        Value::Array(_) => str_list(v, "tag")?,
        _ => return Err(ApiError::invalid("tags must be a list of strings, or null.")),
    };
    let mut out = vec![];
    for t in raw {
        let t = clean_name("tag", &t)?;
        if !out.contains(&t) {
            out.push(t);
        }
    }
    let max = db.cfg().max_tags;
    if max > 0 && out.len() > max {
        return Err(ApiError::bad("TAG_LIMIT", format!("This record has {} tags; this server allows {max} per record (max_tags).", out.len())));
    }
    Ok(Some(out))
}

pub fn parse_restrictions(v: &Value) -> ApiResult<Vec<Restriction>> {
    let items: Vec<&Value> = match v {
        Value::Null => return Ok(vec![]),
        Value::Array(a) => a.iter().collect(),
        _ => return Err(ApiError::invalid("referencing_index_restrictions must be a list, or null.")),
    };
    let mut out = vec![];
    for it in items {
        let o = it.as_object().ok_or_else(|| ApiError::invalid("Every referencing index restriction must be an object."))?;
        let name = clean_name("index name", req_str(o, "name")?)?;
        let value = match o.get("value") {
            None | Some(Value::Null) => None,
            Some(v) => Some(to_val(v, "restriction value")?),
        };
        let range = match o.get("range") {
            None | Some(Value::Null) => None,
            Some(v) => Some(to_val(v, "restriction range")?),
        };
        let condition = match opt_str(o, "condition")? {
            None => None,
            Some(c) => Some(norm_cond(c, true)?.to_string()),
        };
        if let (Some(a), Some(b)) = (&value, &range) {
            if a.type_byte() != b.type_byte() {
                return Err(ApiError::invalid("A restriction's range must be the same type as its value."));
            }
        }
        out.push(Restriction { name, value, range: if condition.is_some() { None } else { range }, condition });
    }
    Ok(out)
}

/// Accepts the symbols and the names; returns the symbol.
pub fn norm_cond(c: &str, allow_ne: bool) -> ApiResult<&'static str> {
    Ok(match c {
        "=" | "eq" | "==" => "=",
        ">" | "gt" => ">",
        "<" | "lt" => "<",
        ">=" | "gte" => ">=",
        "<=" | "lte" => "<=",
        "!=" | "ne" if allow_ne => "!=",
        _ => {
            return Err(ApiError::invalid(format!(
                "Unknown condition \"{c}\". Use =, >, <, >= (starts with, for strings) or <= (ends with, for strings)."
            )))
        }
    })
}

fn parse_edit(db: &Db, body: &Value) -> ApiResult<Edit> {
    let b = obj(body)?;
    let cfg = match b.get("config") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(o)) => o.clone(),
        Some(_) => return Err(ApiError::invalid("config must be an object.")),
    };
    only_keys(&cfg, "config", &["record_id", "collection", "index", "tags", "reference", "unique_id", "source", "readonly", "notification"])?;
    let mut e = Edit::default();
    if let Some(id) = opt_str(&cfg, "record_id")? {
        e.record_id = Some(id.to_string());
    }
    match cfg.get("collection") {
        None | Some(Value::Null) => {}
        Some(Value::String(s)) => e.collection = Some(clean_name("collection name", s)?),
        Some(Value::Object(o)) => {
            only_keys(o, "collection", &["name", "access_level", "subscription"])?;
            if let Some(n) = opt_str(o, "name")? {
                e.collection = Some(clean_name("collection name", n)?);
            }
            if let Some(l) = o.get("access_level").filter(|v| !v.is_null()) {
                e.level = Some(parse_level(l)?);
            }
            match o.get("subscription") {
                None => {}
                Some(Value::Null) => e.sub = Some(SubCfg::default()),
                Some(Value::Object(s)) => {
                    e.sub = Some(SubCfg {
                        is_subscription_record: opt_bool(s, "is_subscription_record")?.unwrap_or(false),
                        upload_to_feed: opt_bool(s, "upload_to_feed")?.unwrap_or(false),
                        notify_subscribers: opt_bool(s, "notify_subscribers")?.unwrap_or(false),
                        feed_referencing_records: opt_bool(s, "feed_referencing_records")?.unwrap_or(false),
                        notify_referencing_records: opt_bool(s, "notify_referencing_records")?.unwrap_or(false),
                    })
                }
                Some(_) => return Err(ApiError::invalid("collection.subscription must be an object or null.")),
            }
        }
        Some(_) => return Err(ApiError::invalid("collection must be a name or {name, access_level, subscription}.")),
    }
    if let Some(v) = cfg.get("index") {
        e.index = Some(parse_index(db, v)?);
    }
    if let Some(v) = cfg.get("tags") {
        e.tags = Some(parse_tags(db, v)?);
    }
    match cfg.get("reference") {
        None => {}
        Some(Value::Null) => e.reference = Some(None),
        Some(Value::String(s)) => e.reference = Some(Some(s.clone())),
        Some(Value::Object(o)) => {
            // the object form says which kind of id it holds; the plain string may be either
            let id = match (opt_str(o, "record_id")?, opt_str(o, "unique_id")?) {
                (Some(r), _) => Some(record_by_id(db, r)?.ok_or_else(|| ApiError::not_found(format!("The referenced record {r} does not exist.")))?),
                (None, Some(u)) => Some(record_by_unique_id(db, u)?.ok_or_else(|| ApiError::not_found(format!("No record has the unique id {u}.")))?),
                _ => None,
            };
            e.reference = Some(id);
        }
        Some(_) => return Err(ApiError::invalid("reference must be a record id, a unique id, or null.")),
    }
    match cfg.get("unique_id") {
        None => {}
        Some(Value::Null) => e.unique_id = Some(None),
        Some(Value::String(s)) => e.unique_id = Some(Some(clean_unique_id(s)?)),
        Some(_) => return Err(ApiError::invalid("unique_id must be a string or null.")),
    }
    match cfg.get("source") {
        None => {}
        Some(Value::Null) => e.source_reset = true,
        Some(Value::Object(o)) => e.source = Some(o.clone()),
        Some(_) => return Err(ApiError::invalid("source must be an object or null.")),
    }
    e.readonly = opt_bool(&cfg, "readonly")?;
    match cfg.get("notification") {
        None | Some(Value::Null) => {}
        Some(Value::Object(o)) => {
            e.notification = Some(Notification { title: req_str(o, "title")?.to_string(), body: req_str(o, "body")?.to_string() })
        }
        Some(_) => return Err(ApiError::invalid("notification must be {title, body}.")),
    }
    if let Some(d) = b.get("data") {
        e.data = Some(serde_json::to_vec(d).map_err(|e| ApiError::invalid(e.to_string()))?);
    }
    Ok(e)
}

fn apply_source(src: &mut Source, o: &Map<String, Value>) -> ApiResult<()> {
    only_keys(
        o,
        "source",
        &["referencing_limit", "prevent_multiple_referencing", "can_remove_referencing_records", "only_granted_can_reference", "allow_granted_to_grant_others", "referencing_index_restrictions"],
    )?;
    if o.contains_key("referencing_limit") {
        src.referencing_limit = opt_u64(o, "referencing_limit")?;
    }
    for (k, f) in [
        ("prevent_multiple_referencing", &mut src.prevent_multiple_referencing as &mut bool),
        ("can_remove_referencing_records", &mut src.can_remove_referencing_records),
        ("only_granted_can_reference", &mut src.only_granted_can_reference),
        ("allow_granted_to_grant_others", &mut src.allow_granted_to_grant_others),
    ] {
        if let Some(b) = opt_bool(o, k)? {
            *f = b;
        }
    }
    if let Some(v) = o.get("referencing_index_restrictions") {
        src.referencing_index_restrictions = parse_restrictions(v)?;
    }
    Ok(())
}

fn cmp_val(a: &Val, b: &Val) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Val::Num(x), Val::Num(y)) => x.partial_cmp(y),
        (Val::Str(x), Val::Str(y)) => Some(x.cmp(y)),
        (Val::Bool(x), Val::Bool(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

fn restriction_ok(r: &Restriction, index: &[(String, Val)]) -> bool {
    let Some((_, v)) = index.iter().find(|(n, _)| n == &r.name) else { return false };
    let Some(want) = &r.value else { return true };
    if v.type_byte() != want.type_byte() {
        return false;
    }
    if let Some(hi) = &r.range {
        return matches!(cmp_val(v, want), Some(o) if o.is_ge()) && matches!(cmp_val(v, hi), Some(o) if o.is_le());
    }
    use std::cmp::Ordering::*;
    let ord = cmp_val(v, want);
    match (r.condition.as_deref().unwrap_or("="), v, want) {
        (">=", Val::Str(s), Val::Str(w)) => s.starts_with(w.as_str()),
        ("<=", Val::Str(s), Val::Str(w)) => s.ends_with(w.as_str()),
        ("=", ..) => ord == Some(Equal),
        ("!=", ..) => ord != Some(Equal),
        (">", ..) => ord == Some(Greater),
        ("<", ..) => ord == Some(Less),
        (">=", ..) => matches!(ord, Some(Greater | Equal)),
        ("<=", ..) => matches!(ord, Some(Less | Equal)),
        _ => false,
    }
}

/// The referencing rules of the target record (PLAN.md section 1).
fn check_reference(db: &Db, c: &Caller, target_id: &str, t: &Meta, new_id: &str, new: &Meta) -> ApiResult<()> {
    if c.admin {
        return Ok(());
    }
    if !can_read(db, c, target_id, t, None)? {
        return Err(ApiError::denied(format!("You cannot reference record {target_id}: you cannot read it.")));
    }
    let owner = t.user_id == c.user_id;
    if let Some(lim) = t.source.referencing_limit {
        if lim == 0 {
            return Err(ApiError::bad("REFERENCE_LIMIT", format!("Record {target_id} does not allow references (referencing_limit is 0).")));
        }
        if t.referenced_count >= lim {
            return Err(ApiError::bad("REFERENCE_LIMIT", format!("Record {target_id} already has {} references; its limit is {lim}.", t.referenced_count)));
        }
    }
    if t.source.prevent_multiple_referencing {
        let p = refby_prefix(target_id);
        let mut up = p.clone();
        push_comp(&mut up, &new.user_id);
        let mut other = false;
        db.store.scan_prefix(CF_IX, &up, |k, _| {
            if record_id_of(k) != new_id {
                other = true;
            }
            !other
        })?;
        if other {
            return Err(ApiError::bad("REFERENCE_LIMIT", format!("You already reference record {target_id}, and it allows one reference per user.")));
        }
    }
    if t.source.only_granted_can_reference && !owner && !is_granted(db, target_id, &c.user_id)? {
        return Err(ApiError::denied(format!("Only users granted access to record {target_id} can reference it.")));
    }
    for r in &t.source.referencing_index_restrictions {
        if !restriction_ok(r, &new.index) {
            return Err(ApiError::bad(
                "REFERENCE_RESTRICTED",
                format!("Record {target_id} only accepts references whose index \"{}\" matches its referencing_index_restrictions.", r.name),
            ));
        }
    }
    Ok(())
}

fn idx_event(coll: &str, name: &str, v: &Val, d: i64) -> Event {
    let num = match v {
        Val::Num(n) => *n,
        Val::Bool(b) => *b as u8 as f64,
        Val::Str(_) => 0.0,
    };
    Event::Idx { coll: coll.to_string(), name: name.to_string(), kind: v.type_byte(), num, d }
}

/// Counter events for a record appearing (+1) or going away (-1).
fn count_events(id: &str, m: &Meta, size: i64, d: i64) -> Vec<Event> {
    let mut ev = vec![
        Event::Coll { coll: m.collection.clone(), level: m.level.clone(), d, size: size * d },
        Event::UserRecords { user: m.user_id.clone(), d },
    ];
    for t in &m.tags {
        ev.push(Event::Tag { coll: m.collection.clone(), tag: t.clone(), d });
    }
    for (n, v) in &m.index {
        ev.push(idx_event(&m.collection, n, v, d));
    }
    if let Some(u) = &m.unique_id {
        ev.push(Event::Uniq { id: u.clone(), record_id: id.to_string(), d });
    }
    ev
}

fn change_refcount(db: &Db, b: &mut Batch, target: &str, d: i64) -> ApiResult<Option<Meta>> {
    if let Some((mut tm, data)) = db.load_record(target)? {
        tm.referenced_count = (tm.referenced_count as i64 + d).max(0) as u64;
        b.put(CF_REC, target.as_bytes(), &body::encode(&tm, &data));
        db.push_event(b, &Event::RefCount { record_id: target.to_string() });
        return Ok(Some(tm));
    }
    Ok(None)
}

fn check_level(c: &Caller, level: &str) -> ApiResult<()> {
    if c.admin {
        return Ok(());
    }
    if level == "**" {
        if !c.private_ok() {
            return Err(ApiError::denied("This agent key cannot use private (**) records."));
        }
        return Ok(());
    }
    let n: u8 = level.parse().unwrap_or(99);
    if n > c.eff_level() {
        return Err(ApiError::denied(format!("You can post at access levels up to {:02} (or ** for private); {level} is above that.", c.eff_level())));
    }
    Ok(())
}

pub fn post_record(db: &Db, c: &Caller, body: &Value, ip: &str) -> ApiResult<Value> {
    let e = parse_edit(db, body)?;
    post_edit(db, c, e, ip)
}

fn post_edit(db: &Db, c: &Caller, e: Edit, ip: &str) -> ApiResult<Value> {
    match e.record_id.clone() {
        Some(r) => {
            let id = record_by_id(db, &r)?.ok_or_else(|| ApiError::not_found(format!("Record {r} does not exist.")))?;
            update(db, c, &id, e, ip)
        }
        None => create(db, c, e, ip, None),
    }
}

pub fn import_record(db: &Db, body: &Value, imp: Imported) -> ApiResult<Value> {
    let e = parse_edit(db, body)?;
    create(db, &Caller::admin(), e, &imp.ip.clone(), Some(imp))
}

/// A new record worked out from its request, before any lock is taken.
struct NewRecord {
    id: String,
    m: Meta,
    data: Vec<u8>,
    reference: Option<String>,
    now: u64,
}

/// Checks a new record's request and builds its record (everything but the final id check and the write).
fn prepare_new(db: &Db, c: &Caller, e: Edit, ip: &str, imp: Option<&Imported>) -> ApiResult<NewRecord> {
    c.check_write()?;
    if c.admin && imp.is_none() {
        return Err(ApiError::denied("The admin cannot create records; records belong to users. Use a user account."));
    }
    let coll = e.collection.clone().ok_or_else(|| ApiError::invalid("config.collection is required to create a record."))?;
    c.check_coll(&coll)?;
    let level = e.level.clone().unwrap_or_else(|| "00".into());
    check_level(c, &level)?;
    let now = super::now();
    let (id, user_id, agent, uploaded, updated) = match imp {
        Some(i) => (i.record_id.clone(), i.user_id.clone(), i.agent.clone(), i.uploaded, i.updated),
        None => (ids::new_record_id(&c.user_id, now), c.user_id.clone(), c.agent.as_ref().map(agent_tag), now, now),
    };
    let mut src = Source::default();
    if let Some(o) = &e.source {
        apply_source(&mut src, o)?;
    }
    let reference = match e.reference.clone().flatten() {
        None => None,
        Some(r) => Some(resolve_record_id(db, &r)?.ok_or_else(|| ApiError::not_found(format!("The referenced record {r} does not exist.")))?),
    };
    let m = Meta {
        user_id,
        agent,
        collection: coll,
        level,
        sub: e.sub.clone().unwrap_or_default(),
        index: e.index.clone().flatten().unwrap_or_default(),
        tags: e.tags.clone().flatten().unwrap_or_default(),
        reference: reference.clone(),
        unique_id: e.unique_id.clone().flatten(),
        source: src,
        readonly: e.readonly.unwrap_or(false),
        uploaded,
        updated,
        referenced_count: 0,
        referenced_count_indexed: 0,
        ip: ip.to_string(),
        notification: e.notification.clone(),
    };
    let data = e.data.unwrap_or_else(|| b"null".to_vec());
    Ok(NewRecord { id, m, data, reference, now })
}

/// Puts a new record into a batch: its body, every key that makes it findable, its level in the catalog, its
/// unique id, its counter events and (for a record saved now, not imported) its own feed and notification
/// events. Nothing here can fail, so a batch never holds half a record.
fn put_new(db: &Db, b: &mut Batch, id: &str, m: &Meta, data: &[u8], live: bool) {
    let bytes = body::encode(m, data);
    b.put(CF_REC, id.as_bytes(), &bytes);
    for k in record_keys(id, m, &[]) {
        b.put(CF_IX, &k, b"");
    }
    super::catalog::mark_level(b, &m.collection, &m.level);
    if let Some(u) = &m.unique_id {
        b.put(CF_UNQ, u.as_bytes(), id.as_bytes());
    }
    for ev in count_events(id, m, bytes.len() as i64, 1) {
        db.push_event(b, &ev);
    }
    if live {
        if m.sub.upload_to_feed {
            db.push_event(b, &Event::Feed { record_id: id.to_string(), uploader: m.user_id.clone(), level: m.level.clone(), to: None });
        }
        if m.sub.notify_subscribers {
            if let Some(n) = &m.notification {
                db.push_event(b, &Event::Notify {
                    record_id: id.to_string(),
                    uploader: m.user_id.clone(),
                    collection: m.collection.clone(),
                    title: n.title.clone(),
                    body: n.body.clone(),
                    to: None,
                });
            }
        }
    }
}

fn create(db: &Db, c: &Caller, e: Edit, ip: &str, imp: Option<Imported>) -> ApiResult<Value> {
    let NewRecord { mut id, mut m, data, reference, now } = prepare_new(db, c, e, ip, imp.as_ref())?;
    let ukey = m.unique_id.as_ref().map(|u| format!("u:{u}")).unwrap_or_default();
    // A new record id is 7 characters of time (ms), 5 random and 4 from the uploader, so two records one user
    // saves in the same millisecond can draw the same id. Then a new id is drawn, under the lock, so no other
    // write can take it between the check and the save. An imported record keeps its id: a duplicate is refused.
    let mut tries = 0;
    let _g = loop {
        let g = db.lock_many(&[&id, reference.as_deref().unwrap_or(""), &ukey]);
        if !db.store.exists(CF_REC, id.as_bytes())? {
            break g;
        }
        if imp.is_some() || tries == MAX_ID_TRIES {
            return Err(ApiError::exists(format!("Record {id} already exists.")));
        }
        drop(g);
        tries += 1;
        id = ids::new_record_id(&m.user_id, now);
    };
    if let Some(u) = &m.unique_id {
        if db.store.exists(CF_UNQ, u.as_bytes())? {
            return Err(ApiError::exists(format!("unique_id \"{u}\" is already used by another record.")));
        }
    }
    let mut b = Batch::new(&db.store);
    let mut target_meta = None;
    if let Some(t) = &reference {
        let tm = db.load_meta(t)?.ok_or_else(|| ApiError::not_found(format!("The referenced record {t} does not exist.")))?;
        if imp.is_none() {
            check_reference(db, c, t, &tm, &id, &m)?;
        }
        target_meta = change_refcount(db, &mut b, t, 1)?;
    }
    // records imported before this one may already point at it; a new record's id is new, so nothing does
    let n = if imp.is_some() { db.store.count_prefix(CF_IX, &refby_prefix(&id))? } else { 0 };
    m.referenced_count = n;
    m.referenced_count_indexed = n;

    put_new(db, &mut b, &id, &m, &data, imp.is_none());
    if imp.is_none() {
        if let (Some(t), Some(tm)) = (&reference, &target_meta) {
            if tm.user_id != m.user_id {
                if tm.sub.feed_referencing_records {
                    db.push_event(&mut b, &Event::Feed { record_id: id.clone(), uploader: m.user_id.clone(), level: m.level.clone(), to: Some(tm.user_id.clone()) });
                }
                if tm.sub.notify_referencing_records {
                    let (title, body) = match &m.notification {
                        Some(n) => (n.title.clone(), n.body.clone()),
                        None => ("New reference".to_string(), format!("Record {id} references your record {t}.")),
                    };
                    db.push_event(&mut b, &Event::Notify {
                        record_id: id.clone(),
                        uploader: m.user_id.clone(),
                        collection: m.collection.clone(),
                        title,
                        body,
                        to: Some(tm.user_id.clone()),
                    });
                }
            }
        }
    }
    b.commit()?;
    db.wake_worker();
    Ok(super::json::record_json(&id, &m, &data))
}

fn update(db: &Db, c: &Caller, id: &str, e: Edit, ip: &str) -> ApiResult<Value> {
    c.check_write()?;
    let first = db.load_meta(id)?.ok_or_else(|| ApiError::not_found(format!("Record {id} does not exist.")))?;
    let new_ref_raw = match &e.reference {
        Some(Some(r)) => Some(resolve_record_id(db, r)?.ok_or_else(|| ApiError::not_found(format!("The referenced record {r} does not exist.")))?),
        _ => None,
    };
    let old_u = first.unique_id.clone().map(|u| format!("u:{u}")).unwrap_or_default();
    let new_u = e.unique_id.clone().flatten().map(|u| format!("u:{u}")).unwrap_or_default();
    let _g = db.lock_many(&[
        id,
        first.reference.as_deref().unwrap_or(""),
        new_ref_raw.as_deref().unwrap_or(""),
        &old_u,
        &new_u,
    ]);
    let (old, old_data) = db.load_record(id)?.ok_or_else(|| ApiError::not_found(format!("Record {id} does not exist.")))?;
    if !c.admin && old.user_id != c.user_id {
        return Err(ApiError::denied(format!("Only the uploader of record {id} can change it.")));
    }
    if old.readonly && !c.admin {
        return Err(ApiError::denied(format!("Record {id} is read-only; only the admin can change it.")));
    }
    c.check_coll(&old.collection)?;
    // a key that cannot read private records cannot change them either, nor move them out of private
    if old.is_private() && !c.private_ok() {
        return Err(ApiError::denied("This agent key cannot use private (**) records."));
    }
    let mut m = old.clone();
    if let Some(cn) = &e.collection {
        c.check_coll(cn)?;
        m.collection = cn.clone();
    }
    if let Some(l) = &e.level {
        m.level = l.clone();
    }
    if m.level != old.level {
        check_level(c, &m.level)?;
        if (m.is_private() || old.is_private()) && old.user_id != c.user_id {
            return Err(ApiError::denied("Only the uploader can move a record into or out of private (**)."));
        }
    }
    if let Some(s) = &e.sub {
        m.sub = s.clone();
    }
    if let Some(i) = &e.index {
        m.index = i.clone().unwrap_or_default();
    }
    if let Some(t) = &e.tags {
        m.tags = t.clone().unwrap_or_default();
    }
    if e.reference.is_some() {
        m.reference = new_ref_raw.clone();
    }
    if let Some(u) = &e.unique_id {
        m.unique_id = u.clone();
    }
    if e.source_reset {
        m.source = Source::default();
    }
    if let Some(o) = &e.source {
        apply_source(&mut m.source, o)?;
    }
    if let Some(r) = e.readonly {
        m.readonly = r;
    }
    if e.notification.is_some() {
        m.notification = e.notification.clone();
    }
    m.updated = super::now().max(old.updated + 1);
    m.ip = ip.to_string();
    if let Some(a) = &c.agent {
        m.agent = Some(agent_tag(a));
    } else if !c.admin {
        m.agent = None;
    }
    let data = e.data.clone().unwrap_or(old_data.clone());

    let mut b = Batch::new(&db.store);
    if m.unique_id != old.unique_id {
        if let Some(u) = &m.unique_id {
            if db.store.exists(CF_UNQ, u.as_bytes())? {
                return Err(ApiError::exists(format!("unique_id \"{u}\" is already used by another record.")));
            }
            b.put(CF_UNQ, u.as_bytes(), id.as_bytes());
        }
        if let Some(u) = &old.unique_id {
            b.delete(CF_UNQ, u.as_bytes());
        }
    }
    if m.reference != old.reference {
        if let Some(t) = &m.reference {
            if t == id {
                return Err(ApiError::invalid("A record cannot reference itself."));
            }
            let tm = db.load_meta(t)?.ok_or_else(|| ApiError::not_found(format!("The referenced record {t} does not exist.")))?;
            check_reference(db, c, t, &tm, id, &m)?;
            change_refcount(db, &mut b, t, 1)?;
        }
        if let Some(t) = &old.reference {
            change_refcount(db, &mut b, t, -1)?;
        }
    } else if let (Some(t), false) = (&m.reference, m.index == old.index) {
        // the index changed: the target's restrictions still apply
        if let Some(tm) = db.load_meta(t)? {
            for r in &tm.source.referencing_index_restrictions {
                if !c.admin && !restriction_ok(r, &m.index) {
                    return Err(ApiError::bad("REFERENCE_RESTRICTED", format!("Record {t} requires index \"{}\" to match its restrictions.", r.name)));
                }
            }
        }
    }
    let gs = grantees(db, id)?;
    let old_keys = record_keys(id, &old, &gs);
    let new_keys = record_keys(id, &m, &gs);
    for k in old_keys.difference(&new_keys) {
        b.delete(CF_IX, k);
    }
    for k in new_keys.difference(&old_keys) {
        b.put(CF_IX, k, b"");
    }
    if (&m.collection, &m.level) != (&old.collection, &old.level) {
        super::catalog::mark_level(&mut b, &m.collection, &m.level);
    }
    let old_size = body::encode(&old, &old_data).len() as i64;
    let bytes = body::encode(&m, &data);
    b.put(CF_REC, id.as_bytes(), &bytes);
    for ev in diff_events(id, &old, old_size, &m, bytes.len() as i64) {
        db.push_event(&mut b, &ev);
    }
    b.commit()?;
    db.wake_worker();
    Ok(super::json::record_json(id, &m, &data))
}

fn diff_events(id: &str, old: &Meta, old_size: i64, new: &Meta, new_size: i64) -> Vec<Event> {
    let mut ev = vec![];
    if old.collection == new.collection && old.level == new.level {
        if new_size != old_size {
            ev.push(Event::Coll { coll: new.collection.clone(), level: new.level.clone(), d: 0, size: new_size - old_size });
        }
    } else {
        ev.push(Event::Coll { coll: old.collection.clone(), level: old.level.clone(), d: -1, size: -old_size });
        ev.push(Event::Coll { coll: new.collection.clone(), level: new.level.clone(), d: 1, size: new_size });
    }
    let tags = |m: &Meta| m.tags.iter().map(|t| (m.collection.clone(), t.clone())).collect::<BTreeSet<_>>();
    let (ot, nt) = (tags(old), tags(new));
    for (c, t) in ot.difference(&nt) {
        ev.push(Event::Tag { coll: c.clone(), tag: t.clone(), d: -1 });
    }
    for (c, t) in nt.difference(&ot) {
        ev.push(Event::Tag { coll: c.clone(), tag: t.clone(), d: 1 });
    }
    let idx = |m: &Meta| {
        m.index
            .iter()
            .map(|(n, v)| ((m.collection.clone(), n.clone(), typed(v)), v.clone()))
            .collect::<BTreeMap<_, _>>()
    };
    let (oi, ni) = (idx(old), idx(new));
    for (k, v) in &oi {
        if !ni.contains_key(k) {
            ev.push(idx_event(&k.0, &k.1, v, -1));
        }
    }
    for (k, v) in &ni {
        if !oi.contains_key(k) {
            ev.push(idx_event(&k.0, &k.1, v, 1));
        }
    }
    if old.unique_id != new.unique_id {
        if let Some(u) = &old.unique_id {
            ev.push(Event::Uniq { id: u.clone(), record_id: id.to_string(), d: -1 });
        }
        if let Some(u) = &new.unique_id {
            ev.push(Event::Uniq { id: u.clone(), record_id: id.to_string(), d: 1 });
        }
    }
    ev
}

/// Deletes one record. `system` skips permission checks (cascades, user deletion).
pub fn delete_record(db: &Db, c: &Caller, id: &str, system: bool) -> ApiResult<Meta> {
    if !system {
        c.check_write()?;
    }
    let first = db.load_meta(id)?.ok_or_else(|| ApiError::not_found(format!("Record {id} does not exist.")))?;
    let ukey = first.unique_id.clone().map(|u| format!("u:{u}")).unwrap_or_default();
    let _g = db.lock_many(&[id, first.reference.as_deref().unwrap_or(""), &ukey]);
    let (m, data) = db.load_record(id)?.ok_or_else(|| ApiError::not_found(format!("Record {id} does not exist.")))?;
    if !system && !c.admin {
        c.check_coll(&m.collection)?;
        let own = m.user_id == c.user_id;
        let by_target = match &m.reference {
            Some(t) if !own => db
                .load_meta(t)?
                .map(|tm| tm.user_id == c.user_id && tm.source.can_remove_referencing_records)
                .unwrap_or(false),
            _ => false,
        };
        if !own && !by_target {
            return Err(ApiError::denied(format!("You cannot delete record {id}: only its uploader can.")));
        }
        if m.readonly {
            return Err(ApiError::denied(format!("Record {id} is read-only; only the admin can delete it.")));
        }
    }
    let gs = grantees(db, id)?;
    let mut b = Batch::new(&db.store);
    for k in record_keys(id, &m, &gs) {
        b.delete(CF_IX, &k);
    }
    for g in &gs {
        b.delete(CF_ACC, &access::acc_rec_key(id, g));
        b.delete(CF_ACC, &access::acc_user_key(g, id));
    }
    b.delete(CF_REC, id.as_bytes());
    if let Some(u) = &m.unique_id {
        if db.store.get(CF_UNQ, u.as_bytes())?.as_deref() == Some(id.as_bytes()) {
            b.delete(CF_UNQ, u.as_bytes());
        }
    }
    if let Some(t) = &m.reference {
        change_refcount(db, &mut b, t, -1)?;
    }
    let size = body::encode(&m, &data).len() as i64;
    for ev in count_events(id, &m, size, -1) {
        db.push_event(&mut b, &ev);
    }
    let referenced = db.store.has_prefix(CF_IX, &refby_prefix(id))?;
    db.push_event(&mut b, &Event::Removed { record_id: id.to_string(), meta: m.clone(), cascade: referenced });
    b.commit()?;
    db.wake_worker();
    Ok(m)
}

/// Removes the reference from a record whose target is gone (cascade).
pub fn clear_reference(db: &Db, id: &str) -> ApiResult<()> {
    let _g = db.lock_many(&[id]);
    let Some((old, data)) = db.load_record(id)? else { return Ok(()) };
    if old.reference.is_none() {
        return Ok(());
    }
    let mut m = old.clone();
    m.reference = None;
    let gs = grantees(db, id)?;
    let (ok, nk) = (record_keys(id, &old, &gs), record_keys(id, &m, &gs));
    let mut b = Batch::new(&db.store);
    for k in ok.difference(&nk) {
        b.delete(CF_IX, k);
    }
    for k in nk.difference(&ok) {
        b.put(CF_IX, k, b"");
    }
    b.put(CF_REC, id.as_bytes(), &body::encode(&m, &data));
    b.commit()
}

/// Worker: move a record's `f` keys to its exact referenced_count.
pub fn sync_refcount_keys(db: &Db, id: &str) -> ApiResult<()> {
    let _g = db.lock_many(&[id]);
    let Some((mut m, data)) = db.load_record(id)? else { return Ok(()) };
    if m.referenced_count == m.referenced_count_indexed {
        return Ok(());
    }
    let gs = grantees(db, id)?;
    let mut b = Batch::new(&db.store);
    for k in super::views::refcount_keys(id, &m, &gs, m.referenced_count_indexed) {
        b.delete(CF_IX, &k);
    }
    for k in super::views::refcount_keys(id, &m, &gs, m.referenced_count) {
        b.put(CF_IX, &k, b"");
    }
    m.referenced_count_indexed = m.referenced_count;
    b.put(CF_REC, id.as_bytes(), &body::encode(&m, &data));
    b.commit()
}

/// deleteRecords: by id(s), or by query (one page).
pub fn delete_records(db: &Db, c: &Caller, body: &Value) -> ApiResult<Value> {
    let o = obj(body)?;
    let mut ids: Vec<String> = vec![];
    if let Some(v) = o.get("record_id").filter(|v| !v.is_null()) {
        ids = str_list(v, "record_id")?;
    } else if let Some(v) = o.get("unique_id").filter(|v| !v.is_null()) {
        for u in str_list(v, "unique_id")? {
            if let Some(id) = record_by_unique_id(db, &u)? {
                ids.push(id);
            }
        }
    } else if o.get("query").is_some() {
        let res = super::query::get_records(db, c, body)?;
        for r in res["list"].as_array().cloned().unwrap_or_default() {
            if let Some(id) = r["record_id"].as_str() {
                ids.push(id.to_string());
            }
        }
    } else {
        return Err(ApiError::invalid("deleteRecords needs record_id, unique_id, or a query."));
    }
    let mut deleted = vec![];
    let mut failed = vec![];
    for id in ids {
        match delete_record(db, c, &id, false) {
            Ok(_) => deleted.push(Value::String(id)),
            Err(e) => failed.push(json!({"record_id": id, "code": e.code, "message": e.message})),
        }
    }
    Ok(json!({"deleted": deleted, "failed": failed}))
}

/// New records of one postRecords request, saved together in one write instead of one write each.
///
/// Saving a record costs one write to the store, and most of that cost is the same whether the write holds
/// one record or a thousand. So while a request is the only one writing (Db::busy), its new records wait here
/// and go to the store together, up to BULK_MAX at a time. As soon as another request writes, the waiting
/// records are saved and the rest of the request goes one record at a time, which is what lets the store
/// combine the writes of many requests by itself.
///
/// Only plain new records wait: no reference and no unique id. A reference changes the target's count and is
/// checked against the target's source rules, and a unique id must be checked against every other record,
/// waiting ones included; such a record is saved the usual way, after the waiting ones.
///
/// Each waiting record keeps its lock stripe until the write, so no other request can take its id meanwhile.
/// Stripes are taken without waiting: when one is busy, the waiting records are saved and their stripes let
/// go first, and only then is the stripe waited for. So a request never waits while it holds a stripe, and
/// two requests can never wait for each other.
struct Bulk<'a> {
    db: &'a Db,
    b: Batch<'a>,
    held: HashMap<usize, MutexGuard<'a, ()>>,
    ids: HashSet<String>,
    waiting: Vec<(usize, Value)>,
    /// results by item number, filled as records are saved
    done: Vec<(usize, ApiResult<Value>)>,
}

impl<'a> Bulk<'a> {
    fn new(db: &'a Db) -> Self {
        Bulk { db, b: Batch::new(&db.store), held: HashMap::new(), ids: HashSet::new(), waiting: vec![], done: vec![] }
    }

    /// Whether a record can wait for the group write: a new record with no reference and no unique id.
    fn fits(e: &Edit) -> bool {
        e.record_id.is_none() && matches!(e.reference, None | Some(None)) && matches!(e.unique_id, None | Some(None))
    }

    /// Checks a new record, gives it an id no record has, and puts it with the waiting ones.
    fn add(&mut self, c: &Caller, e: Edit, ip: &str, item: usize) -> ApiResult<()> {
        let db = self.db;
        let NewRecord { mut id, m, data, now, .. } = prepare_new(db, c, e, ip, None)?;
        let mut tries = 0;
        loop {
            let s = db.stripe(&id);
            if !self.held.contains_key(&s) {
                let g = match db.try_lock_stripe(s) {
                    Some(g) => g,
                    None => {
                        self.save();
                        db.lock_stripe(s)
                    }
                };
                self.held.insert(s, g);
            }
            if !self.ids.contains(&id) && !db.store.exists(CF_REC, id.as_bytes())? {
                break;
            }
            if tries == MAX_ID_TRIES {
                return Err(ApiError::exists(format!("Record {id} already exists.")));
            }
            tries += 1;
            id = ids::new_record_id(&m.user_id, now);
        }
        put_new(db, &mut self.b, &id, &m, &data, true);
        self.waiting.push((item, super::json::record_json(&id, &m, &data)));
        self.ids.insert(id);
        if self.waiting.len() >= BULK_MAX {
            self.save();
        }
        Ok(())
    }

    /// Saves the waiting records in one write and lets go of their stripes.
    fn save(&mut self) {
        if !self.waiting.is_empty() {
            let b = std::mem::replace(&mut self.b, Batch::new(&self.db.store));
            match b.commit() {
                Ok(()) => {
                    self.db.wake_worker();
                    self.done.extend(self.waiting.drain(..).map(|(i, v)| (i, Ok(v))));
                }
                Err(e) => {
                    let msg = format!("The record could not be saved: {}", e.message);
                    self.done.extend(self.waiting.drain(..).map(|(i, _)| (i, Err(ApiError::internal(msg.clone())))));
                }
            }
        }
        self.held.clear();
        self.ids.clear();
    }
}

pub fn post_records(db: &Db, c: &Caller, body: &Value, ip: &str) -> ApiResult<Value> {
    let items = body
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::invalid("postRecords needs \"items\": [{data, config}, ...]."))?;
    let mut bulk = Bulk::new(db);
    let mut calm_from = std::time::Instant::now();
    for (i, it) in items.iter().enumerate() {
        // another request writes: save what waits, so it is not kept waiting, and go one record at a time
        // until no other request has written for BULK_CALM
        if db.busy() {
            bulk.save();
            calm_from = std::time::Instant::now() + BULK_CALM;
        }
        let r = match parse_edit(db, it) {
            Ok(e) if Bulk::fits(&e) && std::time::Instant::now() >= calm_from => match bulk.add(c, e, ip, i) {
                Ok(()) => continue,
                Err(e) => Err(e),
            },
            Ok(e) => {
                // saved after the records before it, as if each were saved alone
                bulk.save();
                post_edit(db, c, e, ip)
            }
            Err(e) => Err(e),
        };
        bulk.done.push((i, r));
    }
    bulk.save();
    let mut done = std::mem::take(&mut bulk.done);
    done.sort_by_key(|(i, _)| *i);
    let mut list = vec![];
    let mut failed = vec![];
    for (i, r) in done {
        match r {
            Ok(v) => list.push(v),
            Err(e) => failed.push(json!({"index": i, "code": e.code, "message": e.message})),
        }
    }
    Ok(json!({"list": list, "failed": failed}))
}

// ---------- grants ----------

fn grant_target(db: &Db, c: &Caller, id: &str) -> ApiResult<Meta> {
    c.check_write()?;
    let m = db.load_meta(id)?.ok_or_else(|| ApiError::not_found(format!("Record {id} does not exist.")))?;
    c.check_coll(&m.collection)?;
    if c.admin || m.user_id == c.user_id {
        return Ok(m);
    }
    if m.source.allow_granted_to_grant_others && is_granted(db, id, &c.user_id)? {
        return Ok(m);
    }
    Err(ApiError::denied(format!("Only the uploader of record {id} can grant access to it.")))
}

pub fn grant(db: &Db, c: &Caller, body: &Value, add: bool) -> ApiResult<Value> {
    let o = obj(body)?;
    let id = req_str(o, "record_id")?.to_string();
    let users = str_list(o.get("user_id").unwrap_or(&Value::Null), "user_id")?;
    if users.is_empty() || users.len() > 100 {
        return Err(ApiError::invalid("Give 1 to 100 user ids per call."));
    }
    let _ = grant_target(db, c, &id)?;
    let _g = db.lock_many(&[&id]);
    let m = db.load_meta(&id)?.ok_or_else(|| ApiError::not_found(format!("Record {id} does not exist.")))?;
    let mut b = Batch::new(&db.store);
    let mut changed = vec![];
    for u in users {
        if u == m.user_id {
            continue;
        }
        if add && super::users::get_user(db, &u)?.is_none() {
            return Err(ApiError::not_found(format!("User {u} does not exist.")));
        }
        let has = is_granted(db, &id, &u)?;
        if add && !has {
            let row = json!({"granted_by": c.user_id, "at": super::now()}).to_string();
            b.put(CF_ACC, &access::acc_rec_key(&id, &u), row.as_bytes());
            b.put(CF_ACC, &access::acc_user_key(&u, &id), b"");
            for k in grantee_keys(&id, &m, &u) {
                b.put(CF_IX, &k, b"");
            }
            changed.push(u);
        } else if !add && has {
            b.delete(CF_ACC, &access::acc_rec_key(&id, &u));
            b.delete(CF_ACC, &access::acc_user_key(&u, &id));
            for k in grantee_keys(&id, &m, &u) {
                b.delete(CF_IX, &k);
            }
            changed.push(u);
        }
    }
    b.commit()?;
    Ok(json!({"record_id": id, if add {"granted"} else {"removed"}: changed}))
}

pub fn list_grants(db: &Db, c: &Caller, body: &Value) -> ApiResult<Value> {
    let o = obj(body)?;
    let mut out = vec![];
    if let Some(id) = opt_str(o, "record_id")? {
        let m = db.load_meta(id)?.ok_or_else(|| ApiError::not_found(format!("Record {id} does not exist.")))?;
        if !c.admin && m.user_id != c.user_id && !is_granted(db, id, &c.user_id)? {
            return Err(ApiError::denied(format!("You cannot list who can access record {id}.")));
        }
        for u in grantees(db, id)? {
            out.push(json!({"record_id": id, "user_id": u}));
        }
    } else {
        let user = opt_str(o, "user_id")?.map(String::from).unwrap_or_else(|| c.user_id.clone());
        if !c.admin && user != c.user_id {
            return Err(ApiError::denied("You can only list your own grants."));
        }
        let p = access::acc_user_prefix(&user);
        db.store.scan_prefix(CF_ACC, &p, |k, _| {
            out.push(json!({"record_id": String::from_utf8_lossy(&k[p.len()..]), "user_id": user}));
            true
        })?;
    }
    Ok(json!({"list": out}))
}

pub fn request_private_key(db: &Db, c: &Caller, body: &Value) -> ApiResult<Value> {
    let o = obj(body)?;
    let id = req_str(o, "record_id")?;
    let m = db.load_meta(id)?.ok_or_else(|| ApiError::not_found(format!("Record {id} does not exist.")))?;
    let mut ok = can_read(db, c, id, &m, None)?;
    if !ok {
        if let Some(rid) = opt_str(o, "reference_id")? {
            // the caller can read a record that references this one
            if let Some(rm) = db.load_meta(rid)? {
                ok = rm.reference.as_deref() == Some(id) && can_read(db, c, rid, &rm, None)?;
            }
        }
    }
    if !ok {
        return Err(ApiError::denied(format!("You have no access to record {id}.")));
    }
    let (key, expiry) = access::make_private_key(db, id, &c.user_id);
    Ok(json!({"record_id": id, "private_key": key, "expires": expiry}))
}
