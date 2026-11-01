//! getRecords: one scope + one sort -> key ranges, merged across access levels (PLAN.md section 4).

use super::access::{can_read, is_active_subscriber, parse_level, Caller};
use super::json::*;
use super::write::{norm_cond, record_by_id, record_by_unique_id, resolve_record_id};
use super::Db;
use crate::codec::body::{self, Meta};
use crate::codec::ids::base62_fixed;
use crate::codec::keys::*;
use crate::codec::loose::loosen;
use crate::codec::text::{clean_name, rev_chars, rev_name};
use crate::error::{ApiError, ApiResult};
use crate::store::{CF_IX, CF_REC};
use rocksdb::ReadOptions;
use serde_json::{json, Value};

struct IndexQ {
    name: String,
    value: Option<Val>,
    cond: &'static str,
    range: Option<Val>,
    loose: bool,
}

/// What the planner produced: sort-relative ranges, plus how to drop duplicates.
struct Plan {
    ranges: Vec<(Vec<u8>, Vec<u8>)>,
    /// compound child search: (sort kind, ranges) used to keep one key per record
    dedup: Option<u8>,
}

fn full(p: Vec<u8>) -> (Vec<u8>, Vec<u8>) {
    let e = prefix_end(&p);
    (p, e)
}

fn sep_end(mut v: Vec<u8>) -> Vec<u8> {
    v.push(SEP);
    v.push(END);
    v
}

fn with(mut a: Vec<u8>, b: &[u8]) -> Vec<u8> {
    a.extend_from_slice(b);
    a
}

fn parse_index_q(v: &Value) -> ApiResult<IndexQ> {
    let o = v.as_object().ok_or_else(|| ApiError::invalid("query.index must be {name, value, condition}."))?;
    only_keys(o, "query.index", &["name", "value", "condition", "range", "match"])?;
    let raw_name = req_str(o, "name")?;
    let name = if raw_name.starts_with('$') { raw_name.to_string() } else { clean_name("index name", raw_name)? };
    let value = match o.get("value") {
        None | Some(Value::Null) => None,
        Some(v) => Some(to_val(v, "query.index.value")?),
    };
    let cond = match opt_str(o, "condition")? {
        None => "=",
        Some(c) => norm_cond(c, false)?,
    };
    let range = match o.get("range") {
        None | Some(Value::Null) => None,
        Some(v) => Some(to_val(v, "query.index.range")?),
    };
    let loose = match opt_str(o, "match")? {
        None | Some("exact") => false,
        Some("loose") => true,
        Some(m) => return Err(ApiError::invalid(format!("Unknown match \"{m}\". Use \"exact\" or \"loose\"."))),
    };
    if range.is_some() {
        if cond != "=" {
            return Err(ApiError::invalid("Use either a condition or a range, not both."));
        }
        match (&value, &range) {
            (Some(a), Some(b)) if a.type_byte() == b.type_byte() => {}
            _ => return Err(ApiError::invalid("A range needs a value of the same type (range = from value to range).")),
        }
    }
    if value.is_none() && cond != "=" {
        return Err(ApiError::invalid("A condition needs a value."));
    }
    Ok(IndexQ { name, value, cond, range, loose })
}

fn plan_u64(kind: u8, q: &IndexQ) -> ApiResult<Plan> {
    let num = |v: &Val| match v {
        Val::Num(n) if *n >= 0.0 => Ok(*n as u64),
        _ => Err(ApiError::invalid(format!("{} takes a number 0 or more.", q.name))),
    };
    let k = vec![kind];
    let Some(v) = &q.value else { return Ok(Plan { ranges: vec![full(k)], dedup: None }) };
    let e = num(v)?.to_be_bytes();
    let ke = with(k.clone(), &e);
    let r = if let Some(hi) = &q.range {
        (ke, with(with(k.clone(), &num(hi)?.to_be_bytes()), &[END]))
    } else {
        match q.cond {
            "=" => full(ke),
            ">" => (with(ke, &[END]), with(k, &[END])),
            "<" => (k, ke),
            ">=" => (ke, with(k, &[END])),
            _ => (k, with(ke, &[END])),
        }
    };
    Ok(Plan { ranges: vec![r], dedup: None })
}

fn plan_uploaded(q: &IndexQ) -> ApiResult<Plan> {
    let ts = |v: &Val| match v {
        Val::Num(n) if *n >= 0.0 => Ok(base62_fixed(*n as u64, 7)),
        _ => Err(ApiError::invalid("$uploaded takes a time in milliseconds.")),
    };
    let k = vec![S_REC];
    let Some(v) = &q.value else { return Ok(Plan { ranges: vec![full(k)], dedup: None }) };
    let kt = with(k.clone(), ts(v)?.as_bytes());
    let r = if let Some(hi) = &q.range {
        (kt, with(with(k.clone(), ts(hi)?.as_bytes()), &[END]))
    } else {
        match q.cond {
            "=" => full(kt),
            ">" => (with(kt, &[END]), with(k, &[END])),
            "<" => (k, kt),
            ">=" => (kt, with(k, &[END])),
            _ => (k, with(kt, &[END])),
        }
    };
    Ok(Plan { ranges: vec![r], dedup: None })
}

fn plan_custom(q: &IndexQ) -> ApiResult<Plan> {
    if q.name.ends_with('.') {
        return plan_compound(q);
    }
    let Some(v0) = &q.value else {
        return Ok(Plan { ranges: vec![full(sort_idx_base(S_IDX, &q.name))], dedup: None });
    };
    let (kind, rkind, v, hi) = if q.loose {
        let Val::Str(s) = v0 else { return Err(ApiError::invalid("Loose match works on string values only.")) };
        let l = loosen(s);
        if l.text.is_empty() {
            return Err(ApiError::invalid("The search text is empty after loosening (it had only spaces or punctuation)."));
        }
        let hi = match &q.range {
            Some(Val::Str(h)) => Some(Val::Str(loosen(h).text)),
            _ => None,
        };
        if l.is_initials_query() {
            (S_INI, S_INI_REV, Val::Str(l.initials), hi)
        } else {
            (S_LOOSE, S_LOOSE_REV, Val::Str(l.text), hi)
        }
    } else {
        (S_IDX, S_REV, v0.clone(), q.range.clone())
    };
    let base = sort_idx_base(kind, &q.name);
    let t = typed(&v);
    let ty = v.type_byte();
    let r = if let Some(h) = &hi {
        (sep(with(base.clone(), &t)), sep_end(with(base, &typed(h))))
    } else {
        match (q.cond, &v) {
            ("=", _) => full(sep(with(base, &t))),
            (">", _) => (with(sep(with(base.clone(), &t)), &[END]), with(base, &[ty + 1])),
            ("<", _) => (with(base.clone(), &[ty]), sep(with(base, &t))),
            (">=", Val::Str(_)) => full(with(base, &t)),
            (">=", _) => (sep(with(base.clone(), &t)), with(base, &[ty + 1])),
            ("<=", Val::Str(s)) => full(with(sort_idx_base(rkind, &rev_name(&q.name)), &typed_str(&rev_chars(s)))),
            _ => (with(base.clone(), &[ty]), sep_end(with(base, &t))),
        }
    };
    Ok(Plan { ranges: vec![r], dedup: None })
}

/// `name.` + child segment.
fn plan_compound(q: &IndexQ) -> ApiResult<Plan> {
    if q.loose || q.range.is_some() {
        return Err(ApiError::invalid("A name ending in '.' searches child names; use =, >= or <= with a string."));
    }
    let parent = q.name.as_str();
    let ip = with(vec![S_IDX], parent.as_bytes());
    let child = match &q.value {
        None => return Ok(Plan { ranges: vec![full(ip)], dedup: Some(S_IDX) }),
        Some(Val::Str(s)) => s.clone(),
        Some(_) => return Err(ApiError::invalid("A name ending in '.' takes a child name (a string) as its value.")),
    };
    Ok(match q.cond {
        "=" => {
            let p = with(ip, child.as_bytes());
            Plan { ranges: vec![full(sep(p.clone())), full(with(p, b"."))], dedup: Some(S_IDX) }
        }
        ">=" => Plan { ranges: vec![full(with(ip, child.as_bytes()))], dedup: Some(S_IDX) },
        "<=" => {
            let rp = format!("{}.{}", rev_name(parent.trim_end_matches('.')), rev_chars(&child));
            Plan { ranges: vec![full(with(vec![S_REV], rp.as_bytes()))], dedup: Some(S_REV) }
        }
        _ => return Err(ApiError::invalid("A name ending in '.' searches child names; use =, >= or <=.")),
    })
}

fn sort_idx_base(kind: u8, name: &str) -> Vec<u8> {
    let mut k = vec![kind];
    k.extend_from_slice(name.as_bytes());
    k.push(SEP);
    k
}

fn sep(mut v: Vec<u8>) -> Vec<u8> {
    v.push(SEP);
    v
}

/// Keep a record only at its first matching key in scan order (compound child searches).
fn dedup_keep(m: &Meta, kind: u8, ranges: &[(Vec<u8>, Vec<u8>)], this: &[u8], asc: bool) -> bool {
    let mut parts: Vec<Vec<u8>> = vec![];
    for (n, v) in &m.index {
        let p = if kind == S_IDX {
            sort_idx(S_IDX, n, &typed(v))
        } else {
            let tv = match v {
                Val::Str(s) => typed_str(&rev_chars(s)),
                _ => typed(v),
            };
            sort_idx(S_REV, &rev_name(n), &tv)
        };
        if ranges.iter().any(|(lo, hi)| p.as_slice() >= lo.as_slice() && p.as_slice() < hi.as_slice()) {
            parts.push(p);
        }
    }
    let best = if asc { parts.iter().min() } else { parts.iter().max() };
    best.map(|b| b.as_slice() == this).unwrap_or(true)
}

pub fn record_from_bytes(id: &str, bytes: &[u8]) -> Option<(Meta, Value)> {
    let (m, d) = body::decode(bytes)?;
    let j = super::json::record_json(id, &m, d);
    Some((m, j))
}

pub fn get_records(db: &Db, c: &Caller, body: &Value) -> ApiResult<Value> {
    let q = query_of(body, true)?;
    let qo = q.as_object().ok_or_else(|| ApiError::invalid("query must be an object."))?;
    only_keys(qo, "query", &["record_id", "unique_id", "collection", "index", "tag", "reference", "private_key"])?;
    let f = fetch_opts(body, true)?;
    let private_key = opt_str(qo, "private_key")?;

    // direct lookups
    let direct = match (opt_str(qo, "record_id")?, opt_str(qo, "unique_id")?) {
        (Some(r), _) => Some((record_by_id(db, r)?, format!("Record {r} does not exist."))),
        (None, Some(u)) => Some((record_by_unique_id(db, u)?, format!("No record has the unique id {u}."))),
        _ => None,
    };
    if let Some((found, missing)) = direct {
        let id = found.ok_or_else(|| ApiError::not_found(missing.clone()))?;
        let (m, data) = db.load_record(&id)?.ok_or_else(|| ApiError::not_found(missing))?;
        if !can_read(db, c, &id, &m, private_key)? {
            return Err(ApiError::denied(format!("You have no access to record {id}.")));
        }
        return Ok(page(vec![super::json::record_json(&id, &m, &data)], None, true));
    }

    // scope
    let (coll, level, sub) = match qo.get("collection") {
        Some(Value::String(s)) => (clean_name("collection name", s)?, None, None),
        Some(Value::Object(o)) => (
            clean_name("collection name", req_str(o, "name")?)?,
            o.get("access_level").filter(|v| !v.is_null()).map(parse_level).transpose()?,
            opt_str(o, "subscription")?.map(String::from),
        ),
        _ => return Err(ApiError::invalid("query.collection is required (a name, or {name, access_level, subscription}).")),
    };
    c.check_coll(&coll)?;
    let tag = opt_str(qo, "tag")?.map(|t| clean_name("tag", t)).transpose()?.unwrap_or_default();
    let mut index = qo.get("index").filter(|v| !v.is_null()).map(parse_index_q).transpose()?;

    let (mut view, mut owner) = (V_COLL, String::new());
    match qo.get("reference") {
        None | Some(Value::Null) => {}
        Some(Value::String(s)) => {
            view = V_REF;
            owner = resolve_record_id(db, s)?.ok_or_else(|| ApiError::not_found(format!("The referenced record {s} does not exist.")))?;
        }
        Some(Value::Object(o)) => {
            if let Some(u) = opt_str(o, "user_id")? {
                view = V_USER;
                owner = u.to_string();
            } else if let Some(r) = opt_str(o, "record_id")? {
                view = V_REF;
                owner = record_by_id(db, r)?.ok_or_else(|| ApiError::not_found(format!("The referenced record {r} does not exist.")))?;
            } else if let Some(u) = opt_str(o, "unique_id")? {
                view = V_REF;
                owner = record_by_unique_id(db, u)?.ok_or_else(|| ApiError::not_found(format!("No record has the unique id {u}.")))?;
            }
        }
        Some(_) => return Err(ApiError::invalid("query.reference must be a record id, unique id, or {user_id}.")),
    }
    if let Some(ix) = &index {
        if ix.name == "$user_id" {
            let Some(Val::Str(u)) = &ix.value else { return Err(ApiError::invalid("$user_id takes a user id.")) };
            view = V_USER;
            owner = u.clone();
            index = None;
        }
    }
    let sub = sub.unwrap_or_default();
    if !sub.is_empty() && !c.admin && sub != c.user_id && !is_active_subscriber(db, &sub, &c.user_id)? {
        return Err(ApiError::denied(format!("These are subscription records of user {sub}; subscribe to them first.")));
    }

    // levels
    let mk = |view: u8, owner: &str, lvl: &str| Scope {
        view,
        owner: owner.to_string(),
        coll: coll.clone(),
        lvl: lvl.to_string(),
        sub: sub.clone(),
        tag: tag.clone(),
    };
    let private_scope = |out: &mut Vec<Scope>| {
        if c.admin {
            out.push(mk(view, &owner, "**"));
        } else if c.private_ok() {
            if view == V_COLL {
                out.push(mk(V_PRIV, &c.user_id, "**"));
            } else {
                out.push(mk(view, &owner, "**"));
            }
        }
    };
    let mut scopes = vec![];
    match &level {
        Some(l) if l == "**" => {
            if !c.private_ok() {
                return Err(ApiError::denied("This agent key cannot read private (**) records."));
            }
            private_scope(&mut scopes);
        }
        Some(l) => scopes.push(mk(view, &owner, l)),
        None => {
            // only the levels this collection uses (see catalog::k_level), up to the caller's own
            let top = c.eff_level();
            for l in super::catalog::levels(db, &coll)? {
                if l.parse::<u8>().is_ok_and(|n| n <= top) {
                    scopes.push(mk(view, &owner, &l));
                }
            }
            private_scope(&mut scopes);
        }
    }

    // sort + ranges
    let plan = match &index {
        None => Plan { ranges: vec![full(vec![S_REC])], dedup: None },
        Some(ix) => match ix.name.as_str() {
            "$uploaded" => plan_uploaded(ix)?,
            "$updated" => plan_u64(S_UPD, ix)?,
            "$referenced_count" => plan_u64(S_REFD, ix)?,
            n if n.starts_with('$') => return Err(ApiError::invalid(format!("Unknown built-in index {n}. Use $uploaded, $updated, $referenced_count or $user_id."))),
            _ => plan_custom(ix)?,
        },
    };

    let (list, cursor, end) = run(db, &scopes, &plan, f.ascending, f.limit, f.start_key.as_deref(), |id, m| {
        can_read(db, c, id, m, private_key)
    })?;
    Ok(page(list, cursor, end))
}

/// Scans the ranges in every scope, merges them in order, and returns one page.
fn run(
    db: &Db,
    scopes: &[Scope],
    plan: &Plan,
    asc: bool,
    limit: usize,
    cursor: Option<&[u8]>,
    allow: impl Fn(&str, &Meta) -> ApiResult<bool>,
) -> ApiResult<(Vec<Value>, Option<Vec<u8>>, bool)> {
    let store = &db.store;
    let snap = store.db.snapshot();
    let cf = store.cf(CF_IX);
    let mut srcs = vec![];
    for s in scopes {
        let sb = s.bytes();
        for (lo, hi) in &plan.ranges {
            let mut l = cat(&sb, lo);
            let mut h = cat(&sb, hi);
            if let Some(cur) = cursor {
                let ck = cat(&sb, cur);
                if asc {
                    let after = with(ck, &[0]);
                    if after > l {
                        l = after;
                    }
                } else if ck < h {
                    h = ck;
                }
            }
            if l >= h {
                continue;
            }
            let mut ro = ReadOptions::default();
            ro.set_snapshot(&snap);
            ro.set_iterate_lower_bound(l);
            ro.set_iterate_upper_bound(h);
            let mut it = store.db.raw_iterator_cf_opt(cf, ro);
            if asc {
                it.seek_to_first();
            } else {
                it.seek_to_last();
            }
            srcs.push((it, sb.len()));
        }
    }

    let mut out = vec![];
    let mut last: Option<Vec<u8>> = None;
    loop {
        let want = (limit - out.len()).max(16);
        let mut cand: Vec<(Vec<u8>, String)> = Vec::with_capacity(want);
        let mut exhausted = false;
        while cand.len() < want {
            let mut best: Option<usize> = None;
            for (i, (it, pl)) in srcs.iter().enumerate() {
                if !it.valid() {
                    continue;
                }
                let k = &it.key().unwrap()[*pl..];
                best = match best {
                    None => Some(i),
                    Some(b) => {
                        let bk = &srcs[b].0.key().unwrap()[srcs[b].1..];
                        if (asc && k < bk) || (!asc && k > bk) {
                            Some(i)
                        } else {
                            Some(b)
                        }
                    }
                };
            }
            let Some(b) = best else {
                exhausted = true;
                break;
            };
            let (it, pl) = &mut srcs[b];
            let key = it.key().unwrap();
            cand.push((key[*pl..].to_vec(), record_id_of(key).to_string()));
            if asc {
                it.next();
            } else {
                it.prev();
            }
        }
        for (it, _) in &srcs {
            it.status()?;
        }
        if cand.is_empty() {
            return Ok((out, last, true));
        }
        let keys: Vec<Vec<u8>> = cand.iter().map(|(_, id)| id.as_bytes().to_vec()).collect();
        let bodies = store.multi_get(CF_REC, &keys)?;
        let n = cand.len();
        for (i, ((suffix, id), b)) in cand.into_iter().zip(bodies).enumerate() {
            let Some(bytes) = b else { continue };
            let Some((m, j)) = record_from_bytes(&id, &bytes) else { continue };
            if let Some(kind) = plan.dedup {
                let sort_part = &suffix[..suffix.len() - RECORD_ID_LEN];
                if !dedup_keep(&m, kind, &plan.ranges, sort_part, asc) {
                    continue;
                }
            }
            if !allow(&id, &m)? {
                continue;
            }
            out.push(j);
            last = Some(suffix);
            if out.len() >= limit {
                let end = exhausted && i == n - 1;
                return Ok((out, last, end));
            }
        }
        if exhausted {
            return Ok((out, last, true));
        }
    }
}

/// Admin listing: everything, or by collection / user / level.
pub fn admin_list(db: &Db, body: &Value) -> ApiResult<Value> {
    let o = obj(body)?;
    let f = fetch_opts(body, false)?;
    let coll = opt_str(o, "collection")?.map(String::from);
    let user = opt_str(o, "user_id")?.map(String::from);
    let level = o.get("access_level").filter(|v| !v.is_null()).map(parse_level).transpose()?;
    let ok = |_: &str, m: &Meta| -> ApiResult<bool> {
        Ok(coll.as_ref().map(|c| &m.collection == c).unwrap_or(true) && level.as_ref().map(|l| &m.level == l).unwrap_or(true))
    };
    // by user: the owner listing; otherwise every record, newest first by default
    let mut list = vec![];
    let mut last = None;
    let mut end = true;
    let (cf, prefix) = match &user {
        Some(u) => (CF_IX, owner_prefix(u)),
        None => (CF_REC, vec![]),
    };
    let (mut lo, mut hi) = (prefix.clone(), prefix_end(&prefix));
    if let Some(c) = &f.start_key {
        if f.ascending {
            lo = with(c.clone(), &[0]);
        } else {
            hi = c.clone();
        }
    }
    let mut err = None;
    db.store.scan(cf, &lo, &hi, f.ascending, |k, v| {
        let (id, bytes) = if cf == CF_IX {
            let id = record_id_of(k).to_string();
            match db.store.get(CF_REC, id.as_bytes()) {
                Ok(Some(b)) => (id, b),
                Ok(None) => return true,
                Err(e) => {
                    err = Some(e);
                    return false;
                }
            }
        } else {
            (String::from_utf8_lossy(k).into_owned(), v.to_vec())
        };
        let Some((m, j)) = record_from_bytes(&id, &bytes) else { return true };
        if !matches!(ok(&id, &m), Ok(true)) {
            return true;
        }
        if list.len() >= f.limit {
            end = false;
            return false;
        }
        list.push(j);
        last = Some(k.to_vec());
        true
    })?;
    if let Some(e) = err {
        return Err(e);
    }
    Ok(page(list, last, end))
}

pub fn record_json_by_id(db: &Db, id: &str) -> ApiResult<Value> {
    let (m, d) = db.load_record(id)?.ok_or_else(|| ApiError::not_found(format!("Record {id} does not exist.")))?;
    Ok(json!(super::json::record_json(id, &m, &d)))
}
