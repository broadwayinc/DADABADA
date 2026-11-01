//! Catalog: collections, tags, indexes, unique ids. Counters are written only by the worker.

use super::access::Caller;
use super::json::*;
use super::Db;
use crate::codec::keys::{enc_num, num_json, prefix_end, push_comp, END};
use crate::codec::text::{clean_name, rev_chars};
use crate::error::{ApiError, ApiResult};
use crate::store::{Batch, CF_CAT, CF_IX, CF_UNQ};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CollStat {
    pub records: i64,
    pub size: i64,
    pub levels: BTreeMap<String, i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TagStat {
    pub records: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IdxStat {
    pub records: i64,
    pub num_count: i64,
    pub num_total: f64,
    pub bool_count: i64,
    pub bool_total: i64,
    pub str_count: i64,
}

impl IdxStat {
    pub fn avg_num(&self) -> f64 {
        if self.num_count > 0 { self.num_total / self.num_count as f64 } else { 0.0 }
    }
    pub fn avg_bool(&self) -> f64 {
        if self.bool_count > 0 { self.bool_total as f64 / self.bool_count as f64 } else { 0.0 }
    }
    /// (order code, value) pairs kept as order keys for getIndexes order.by
    pub fn order_values(&self) -> [(u8, f64); 8] {
        [
            (b'a', self.avg_num()),
            (b'b', self.num_total),
            (b'c', self.num_count as f64),
            (b'd', self.avg_bool()),
            (b'e', self.bool_total as f64),
            (b'f', self.bool_count as f64),
            (b'g', self.str_count as f64),
            (b'h', self.records as f64),
        ]
    }
}

pub fn order_code(by: &str) -> ApiResult<u8> {
    Ok(match by {
        "average_number" => b'a',
        "total_number" => b'b',
        "number_count" => b'c',
        "average_bool" => b'd',
        "total_bool" => b'e',
        "bool_count" => b'f',
        "string_count" => b'g',
        "number_of_records" => b'h',
        "index_name" => 0,
        _ => {
            return Err(ApiError::invalid(
                "order.by must be index_name, number_of_records, average_number, total_number, number_count, average_bool, total_bool, bool_count or string_count.",
            ))
        }
    })
}

// key builders
pub fn k_coll(c: &str) -> Vec<u8> {
    let mut k = vec![b'c'];
    k.extend_from_slice(c.as_bytes());
    k
}
pub fn k_coll_rev(c: &str) -> Vec<u8> {
    let mut k = vec![b'C'];
    k.extend_from_slice(rev_chars(c).as_bytes());
    k
}
fn coll_pre(t: u8, c: &str) -> Vec<u8> {
    let mut k = vec![t];
    push_comp(&mut k, c);
    k
}

/// Level marks: `L coll\0 level`, written with every record saved in a collection at a level and never
/// removed. A query opens key ranges only for the levels its collection uses, so a caller at level 99
/// does not seek through 100 levels when the collection uses three. An emptied level keeps its mark and
/// costs one empty seek, as every level did before.
pub fn k_level(c: &str, lvl: &str) -> Vec<u8> {
    let mut k = coll_pre(b'L', c);
    k.extend_from_slice(lvl.as_bytes());
    k
}

/// Marks the record's collection and level as used, in the record's own batch.
pub fn mark_level(b: &mut Batch, c: &str, lvl: &str) {
    if lvl != "**" {
        b.put(CF_CAT, &k_level(c, lvl), b"");
    }
}

/// The levels a collection uses, lowest first.
pub fn levels(db: &Db, c: &str) -> ApiResult<Vec<String>> {
    let p = coll_pre(b'L', c);
    let mut out = vec![];
    db.store.scan_prefix(CF_CAT, &p, |k, _| {
        out.push(String::from_utf8_lossy(&k[p.len()..]).into_owned());
        true
    })?;
    Ok(out)
}

/// Writes the level marks of a data folder made before they existed, from the query keys themselves:
/// one seek per collection and level that has records.
pub fn build_level_marks(db: &Db) -> ApiResult<()> {
    let store = &db.store;
    let mut b = Batch::new(store);
    let mut it = store.db.raw_iterator_cf(store.cf(CF_IX));
    it.seek(b"C");
    while it.valid() {
        let k = it.key().unwrap();
        if k.first() != Some(&b'C') {
            break;
        }
        // C coll\0 level\0 ...: collection and level are the first two parts
        let mut parts = k[1..].splitn(3, |&x| x == 0);
        let (Some(coll), Some(lvl)) = (parts.next(), parts.next()) else { break };
        let (coll, lvl) = (String::from_utf8_lossy(coll).into_owned(), String::from_utf8_lossy(lvl).into_owned());
        mark_level(&mut b, &coll, &lvl);
        let mut next = vec![b'C'];
        push_comp(&mut next, &coll);
        push_comp(&mut next, &lvl);
        it.seek(prefix_end(&next));
    }
    it.status()?;
    b.commit()
}
pub fn k_tag(c: &str, t: &str) -> Vec<u8> {
    let mut k = coll_pre(b't', c);
    k.extend_from_slice(t.as_bytes());
    k
}
pub fn k_tag_rev(c: &str, t: &str) -> Vec<u8> {
    let mut k = coll_pre(b'T', c);
    k.extend_from_slice(rev_chars(t).as_bytes());
    k
}
pub fn k_idx(c: &str, n: &str) -> Vec<u8> {
    let mut k = coll_pre(b'i', c);
    k.extend_from_slice(n.as_bytes());
    k
}
pub fn k_idx_rev(c: &str, n: &str) -> Vec<u8> {
    let mut k = coll_pre(b'I', c);
    k.extend_from_slice(rev_chars(n).as_bytes());
    k
}
pub fn k_order(c: &str, code: u8, v: f64, n: &str) -> Vec<u8> {
    let mut k = coll_pre(b'o', c);
    k.push(code);
    k.extend_from_slice(&enc_num(v));
    k.extend_from_slice(n.as_bytes());
    k
}
pub fn k_uniq_rev(u: &str) -> Vec<u8> {
    let mut k = vec![b'U'];
    k.extend_from_slice(rev_chars(u).as_bytes());
    k
}

fn get_json<T: for<'de> Deserialize<'de> + Default>(db: &Db, k: &[u8]) -> ApiResult<Option<T>> {
    Ok(db.store.get(CF_CAT, k)?.map(|v| serde_json::from_slice(&v).unwrap_or_default()))
}

// ---------- writers (worker only) ----------

pub fn apply_coll(db: &Db, b: &mut Batch, coll: &str, d: i64, size: i64, levels: &BTreeMap<String, i64>) -> ApiResult<()> {
    let k = k_coll(coll);
    let mut s: CollStat = get_json(db, &k)?.unwrap_or_default();
    let new = s.records == 0 && !db.store.exists(CF_CAT, &k)?;
    s.records += d;
    s.size += size;
    for (l, n) in levels {
        *s.levels.entry(l.clone()).or_default() += n;
    }
    s.levels.retain(|_, n| *n > 0);
    if s.records <= 0 {
        b.delete(CF_CAT, &k);
        b.delete(CF_CAT, &k_coll_rev(coll));
    } else {
        b.put(CF_CAT, &k, &serde_json::to_vec(&s).unwrap());
        if new {
            b.put(CF_CAT, &k_coll_rev(coll), b"");
        }
    }
    Ok(())
}

pub fn apply_tag(db: &Db, b: &mut Batch, coll: &str, tag: &str, d: i64) -> ApiResult<()> {
    let k = k_tag(coll, tag);
    let mut s: TagStat = get_json(db, &k)?.unwrap_or_default();
    s.records += d;
    if s.records <= 0 {
        b.delete(CF_CAT, &k);
        b.delete(CF_CAT, &k_tag_rev(coll, tag));
    } else {
        b.put(CF_CAT, &k, &serde_json::to_vec(&s).unwrap());
        b.put(CF_CAT, &k_tag_rev(coll, tag), b"");
    }
    Ok(())
}

pub fn apply_idx(db: &Db, b: &mut Batch, coll: &str, name: &str, delta: &IdxStat) -> ApiResult<()> {
    let k = k_idx(coll, name);
    let old: Option<IdxStat> = get_json(db, &k)?;
    let mut s = old.clone().unwrap_or_default();
    if let Some(o) = &old {
        for (code, v) in o.order_values() {
            b.delete(CF_CAT, &k_order(coll, code, v, name));
        }
    }
    s.records += delta.records;
    s.num_count += delta.num_count;
    s.num_total += delta.num_total;
    s.bool_count += delta.bool_count;
    s.bool_total += delta.bool_total;
    s.str_count += delta.str_count;
    if s.records <= 0 {
        b.delete(CF_CAT, &k);
        b.delete(CF_CAT, &k_idx_rev(coll, name));
    } else {
        b.put(CF_CAT, &k, &serde_json::to_vec(&s).unwrap());
        b.put(CF_CAT, &k_idx_rev(coll, name), b"");
        for (code, v) in s.order_values() {
            b.put(CF_CAT, &k_order(coll, code, v, name), b"");
        }
    }
    Ok(())
}

pub fn apply_uniq(b: &mut Batch, id: &str, record_id: &str, d: i64) {
    if d > 0 {
        b.put(CF_CAT, &k_uniq_rev(id), record_id.as_bytes());
    } else {
        b.delete(CF_CAT, &k_uniq_rev(id));
    }
}

// ---------- listings ----------

/// Name condition -> (lo, hi, reversed). `>=` starts with, `<=` ends with.
fn name_range(fwd: &[u8], rev: &[u8], v: Option<&str>, cond: &str) -> (Vec<u8>, Vec<u8>, bool) {
    let cat = |a: &[u8], b: &[u8]| [a, b].concat();
    match v {
        None | Some("") => (fwd.to_vec(), prefix_end(fwd), false),
        Some(v) => match cond {
            "=" => (cat(fwd, v.as_bytes()), cat(&cat(fwd, v.as_bytes()), &[0]), false),
            ">" => (cat(&cat(fwd, v.as_bytes()), &[0]), prefix_end(fwd), false),
            "<" => (fwd.to_vec(), cat(fwd, v.as_bytes()), false),
            ">=" => (cat(fwd, v.as_bytes()), cat(&cat(fwd, v.as_bytes()), &[END]), false),
            _ => {
                let p = cat(rev, rev_chars(v).as_bytes());
                let e = prefix_end(&p);
                (p, e, true)
            }
        },
    }
}

fn cond_of(o: &serde_json::Map<String, Value>) -> ApiResult<&'static str> {
    match opt_str(o, "condition")? {
        None => Ok("="),
        Some(c) => super::write::norm_cond(c, false),
    }
}

/// Scans [lo, hi) with paging; `each` turns a key into an item (None = skip).
fn scan_page(
    db: &Db,
    cf: &str,
    lo: Vec<u8>,
    hi: Vec<u8>,
    f: &Fetch,
    mut each: impl FnMut(&[u8], &[u8]) -> ApiResult<Option<Value>>,
) -> ApiResult<Value> {
    let (mut lo, mut hi) = (lo, hi);
    if let Some(c) = &f.start_key {
        if f.ascending {
            let mut a = c.clone();
            a.push(0);
            if a > lo {
                lo = a;
            }
        } else if c < &hi {
            hi = c.clone();
        }
    }
    let mut list = vec![];
    let mut last = None;
    let mut end = true;
    let mut err = None;
    if lo < hi {
        db.store.scan(cf, &lo, &hi, f.ascending, |k, v| match each(k, v) {
            Ok(None) => true,
            Ok(Some(j)) => {
                if list.len() >= f.limit {
                    end = false;
                    return false;
                }
                list.push(j);
                last = Some(k.to_vec());
                true
            }
            Err(e) => {
                err = Some(e);
                false
            }
        })?;
    }
    if let Some(e) = err {
        return Err(e);
    }
    Ok(page(list, last, end))
}

fn coll_json(c: &Caller, name: &str, s: &CollStat) -> Value {
    let levels: BTreeMap<&String, &i64> = s
        .levels
        .iter()
        .filter(|(l, _)| c.admin || (l.as_str() != "**" && l.parse::<u8>().map(|n| n <= c.eff_level()).unwrap_or(false)))
        .collect();
    json!({"collection": name, "number_of_records": s.records, "size": s.size, "number_of_records_by_level": levels})
}

pub fn get_collections(db: &Db, c: &Caller, body: &Value) -> ApiResult<Value> {
    let q = &query_of(body, false)?;
    let o = obj(q)?;
    let f = fetch_opts(body, true)?;
    let name = opt_str(o, "collection")?.map(|s| clean_name("collection name", s)).transpose()?;
    let (lo, hi, rev) = name_range(b"c", b"C", name.as_deref(), cond_of(o)?);
    scan_page(db, CF_CAT, lo, hi, &f, |k, v| {
        let coll = if rev { rev_chars(&String::from_utf8_lossy(&k[1..])) } else { String::from_utf8_lossy(&k[1..]).into_owned() };
        if !c.coll_ok(&coll) {
            return Ok(None);
        }
        let s: CollStat = if rev { get_json(db, &k_coll(&coll))?.unwrap_or_default() } else { serde_json::from_slice(v).unwrap_or_default() };
        if s.records <= 0 {
            return Ok(None);
        }
        Ok(Some(coll_json(c, &coll, &s)))
    })
}

pub fn get_tags(db: &Db, c: &Caller, body: &Value) -> ApiResult<Value> {
    let q = &query_of(body, false)?;
    let o = obj(q)?;
    let f = fetch_opts(body, true)?;
    let coll = clean_name("collection name", req_str(o, "collection")?)?;
    c.check_coll(&coll)?;
    let tag = opt_str(o, "tag")?.map(|s| clean_name("tag", s)).transpose()?;
    let (fwd, rv) = (coll_pre(b't', &coll), coll_pre(b'T', &coll));
    let (lo, hi, rev) = name_range(&fwd, &rv, tag.as_deref(), cond_of(o)?);
    scan_page(db, CF_CAT, lo, hi, &f, |k, v| {
        let raw = String::from_utf8_lossy(&k[fwd.len()..]).into_owned();
        let tag = if rev { rev_chars(&raw) } else { raw };
        let s: TagStat = if rev { get_json(db, &k_tag(&coll, &tag))?.unwrap_or_default() } else { serde_json::from_slice(v).unwrap_or_default() };
        Ok(Some(json!({"collection": coll, "tag": tag, "number_of_records": s.records})))
    })
}

fn idx_json(coll: &str, name: &str, s: &IdxStat) -> Value {
    json!({
        "collection": coll,
        "index": name,
        "number_of_records": s.records,
        "string_count": s.str_count,
        "number_count": s.num_count,
        "boolean_count": s.bool_count,
        "total_number": num_json(s.num_total),
        "total_bool": s.bool_total,
        "average_number": num_json(s.avg_num()),
        "average_bool": s.avg_bool(),
    })
}

pub fn get_indexes(db: &Db, c: &Caller, body: &Value) -> ApiResult<Value> {
    let q = &query_of(body, false)?;
    let o = obj(q)?;
    let f = fetch_opts(body, true)?;
    let coll = clean_name("collection name", req_str(o, "collection")?)?;
    c.check_coll(&coll)?;
    let index = opt_str(o, "index")?.map(|s| clean_name("index name", s)).transpose()?;
    if let Some(Value::Object(ord)) = o.get("order") {
        let code = order_code(req_str(ord, "by")?)?;
        if code != 0 {
            let base = [coll_pre(b'o', &coll), vec![code]].concat();
            let (lo, hi) = match ord.get("value").filter(|v| !v.is_null()) {
                None => (base.clone(), prefix_end(&base)),
                Some(v) => {
                    let n = v.as_f64().ok_or_else(|| ApiError::invalid("order.value must be a number."))?;
                    let e = [base.clone(), enc_num(n).to_vec()].concat();
                    match super::write::norm_cond(opt_str(ord, "condition")?.unwrap_or("="), false)? {
                        "=" => (e.clone(), prefix_end(&e)),
                        ">" => (prefix_end(&e), prefix_end(&base)),
                        "<" => (base.clone(), e),
                        ">=" => (e, prefix_end(&base)),
                        _ => (base.clone(), prefix_end(&e)),
                    }
                }
            };
            let parent = index.clone().filter(|i| i.ends_with('.'));
            return scan_page(db, CF_CAT, lo, hi, &f, |k, _| {
                let name = String::from_utf8_lossy(&k[base.len() + 8..]).into_owned();
                if let Some(p) = &parent {
                    if !name.starts_with(p.as_str()) {
                        return Ok(None);
                    }
                }
                let s: IdxStat = get_json(db, &k_idx(&coll, &name))?.unwrap_or_default();
                Ok(Some(idx_json(&coll, &name, &s)))
            });
        }
    }
    let (fwd, rv) = (coll_pre(b'i', &coll), coll_pre(b'I', &coll));
    // a name ending in '.' lists its children
    let cond = match cond_of(o)? {
        "=" if index.as_deref().is_some_and(|i| i.ends_with('.')) => ">=",
        c => c,
    };
    let (lo, hi, rev) = name_range(&fwd, &rv, index.as_deref(), cond);
    scan_page(db, CF_CAT, lo, hi, &f, |k, v| {
        let raw = String::from_utf8_lossy(&k[fwd.len()..]).into_owned();
        let name = if rev { rev_chars(&raw) } else { raw };
        let s: IdxStat = if rev { get_json(db, &k_idx(&coll, &name))?.unwrap_or_default() } else { serde_json::from_slice(v).unwrap_or_default() };
        Ok(Some(idx_json(&coll, &name, &s)))
    })
}

/// A unique id is listed only when the caller can read its record (agent key limits included).
fn readable(db: &Db, c: &Caller, record_id: &str) -> ApiResult<bool> {
    match db.load_meta(record_id)? {
        Some(m) => super::access::can_read(db, c, record_id, &m, None),
        None => Ok(false),
    }
}

pub fn get_unique_id(db: &Db, c: &Caller, body: &Value) -> ApiResult<Value> {
    let q = &query_of(body, false)?;
    let o = obj(q)?;
    let f = fetch_opts(body, true)?;
    let u = opt_str(o, "unique_id")?.map(crate::codec::text::nfc);
    let cond = cond_of(o)?;
    if cond == "<=" && u.as_deref().is_some_and(|s| !s.is_empty()) {
        let (lo, hi, _) = name_range(b"", b"U", u.as_deref(), "<=");
        return scan_page(db, CF_CAT, lo, hi, &f, |k, v| {
            let rid = String::from_utf8_lossy(v).into_owned();
            if !readable(db, c, &rid)? {
                return Ok(None);
            }
            Ok(Some(json!({"unique_id": rev_chars(&String::from_utf8_lossy(&k[1..])), "record_id": rid})))
        });
    }
    let (lo, hi, _) = name_range(b"", b"U", u.as_deref(), cond);
    scan_page(db, CF_UNQ, lo, hi, &f, |k, v| {
        let rid = String::from_utf8_lossy(v).into_owned();
        if !readable(db, c, &rid)? {
            return Ok(None);
        }
        Ok(Some(json!({"unique_id": String::from_utf8_lossy(k), "record_id": rid})))
    })
}

pub fn collection_count(db: &Db) -> ApiResult<(u64, i64)> {
    let mut n = 0;
    let mut recs = 0;
    db.store.scan_prefix(CF_CAT, b"c", |_, v| {
        n += 1;
        recs += serde_json::from_slice::<CollStat>(v).map(|s| s.records).unwrap_or(0);
        true
    })?;
    Ok((n, recs))
}
