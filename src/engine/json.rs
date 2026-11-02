//! JSON shapes and small parsing helpers.

use crate::codec::body::{Meta, Restriction};
use crate::codec::keys::Val;
use crate::error::{ApiError, ApiResult};
use serde_json::{json, Map, Value};

pub fn record_json(id: &str, m: &Meta, data: &[u8]) -> Value {
    let data: Value = if data.is_empty() { Value::Null } else { serde_json::from_slice(data).unwrap_or(Value::Null) };
    json!({
        "record_id": id,
        "user_id": m.user_id,
        "agent": m.agent,
        "collection": {
            "name": m.collection,
            "access_level": m.level,
            "subscription": {
                "is_subscription_record": m.sub.is_subscription_record,
                "upload_to_feed": m.sub.upload_to_feed,
                "notify_subscribers": m.sub.notify_subscribers,
                "feed_referencing_records": m.sub.feed_referencing_records,
                "notify_referencing_records": m.sub.notify_referencing_records,
            }
        },
        "index": m.index.iter().map(|(n, v)| json!({"name": n, "value": v.to_json()})).collect::<Vec<_>>(),
        "tags": m.tags,
        "reference": m.reference,
        "unique_id": m.unique_id,
        "source": {
            "referencing_limit": m.source.referencing_limit,
            "prevent_multiple_referencing": m.source.prevent_multiple_referencing,
            "can_remove_referencing_records": m.source.can_remove_referencing_records,
            "only_granted_can_reference": m.source.only_granted_can_reference,
            "allow_granted_to_grant_others": m.source.allow_granted_to_grant_others,
            "referencing_index_restrictions": m.source.referencing_index_restrictions.iter().map(restriction_json).collect::<Vec<_>>(),
        },
        "readonly": m.readonly,
        "referenced_count": m.referenced_count,
        "uploaded": m.uploaded,
        "updated": m.updated,
        "ip": m.ip,
        "data": data,
    })
}

fn restriction_json(r: &Restriction) -> Value {
    json!({
        "name": r.name,
        "value": r.value.as_ref().map(|v| v.to_json()),
        "range": r.range.as_ref().map(|v| v.to_json()),
        "condition": r.condition,
    })
}

/// Refuses keys that are not in `allowed`, so a misplaced setting is never silently ignored.
pub fn only_keys(o: &Map<String, Value>, what: &str, allowed: &[&str]) -> ApiResult<()> {
    for k in o.keys() {
        if !allowed.contains(&k.as_str()) {
            let hint = match (what, k.as_str()) {
                ("config", "access_level" | "subscription") => format!(" Put it inside collection: {{\"collection\": {{\"name\": ..., \"{k}\": ...}}}}."),
                _ => String::new(),
            };
            return Err(ApiError::invalid(format!("Unknown {what} key \"{k}\". Allowed: {}.{hint}", allowed.join(", "))));
        }
    }
    Ok(())
}

/// The `query` of a `{query, fetch}` body. Query fields belong inside `query`; any other top-level key is refused.
/// Without `query`: an empty query when `required` is false, an error otherwise.
pub fn query_of(body: &Value, required: bool) -> ApiResult<Value> {
    let o = obj(body)?;
    if let Some(k) = o.keys().find(|k| *k != "query" && *k != "fetch") {
        return Err(ApiError::invalid(format!("Unknown key \"{k}\". The body is {{\"query\": {{...}}, \"fetch\": {{...}}}}; query fields go inside query.")));
    }
    match o.get("query") {
        Some(Value::Object(_)) => Ok(o["query"].clone()),
        None | Some(Value::Null) if !required => Ok(Value::Object(Map::new())),
        None | Some(Value::Null) => Err(ApiError::invalid("query is required: {\"query\": {\"collection\": ...}}.")),
        Some(_) => Err(ApiError::invalid("query must be an object.")),
    }
}

pub fn obj(v: &Value) -> ApiResult<&Map<String, Value>> {
    v.as_object().ok_or_else(|| ApiError::invalid("The request body must be a JSON object."))
}

pub fn opt_str<'a>(o: &'a Map<String, Value>, k: &str) -> ApiResult<Option<&'a str>> {
    match o.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(ApiError::invalid(format!("\"{k}\" must be a string."))),
    }
}

pub fn req_str<'a>(o: &'a Map<String, Value>, k: &str) -> ApiResult<&'a str> {
    opt_str(o, k)?.ok_or_else(|| ApiError::invalid(format!("\"{k}\" is required.")))
}

pub fn opt_bool(o: &Map<String, Value>, k: &str) -> ApiResult<Option<bool>> {
    match o.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(ApiError::invalid(format!("\"{k}\" must be true or false."))),
    }
}

pub fn opt_u64(o: &Map<String, Value>, k: &str) -> ApiResult<Option<u64>> {
    match o.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_u64().map(Some).ok_or_else(|| ApiError::invalid(format!("\"{k}\" must be a whole number 0 or more."))),
    }
}

/// A string or a list of strings.
pub fn str_list(v: &Value, what: &str) -> ApiResult<Vec<String>> {
    match v {
        Value::String(s) => Ok(vec![s.clone()]),
        Value::Array(a) => a
            .iter()
            .map(|x| x.as_str().map(String::from).ok_or_else(|| ApiError::invalid(format!("Every {what} must be a string."))))
            .collect(),
        _ => Err(ApiError::invalid(format!("{what} must be a string or a list of strings."))),
    }
}

/// JSON value -> index value. Numbers must be finite.
pub fn to_val(v: &Value, what: &str) -> ApiResult<Val> {
    match v {
        Value::Bool(b) => Ok(Val::Bool(*b)),
        Value::Number(n) => {
            let f = n.as_f64().filter(|f| f.is_finite()).ok_or_else(|| ApiError::invalid(format!("{what} is not a usable number.")))?;
            Ok(Val::Num(f))
        }
        Value::String(s) => Ok(Val::Str(crate::codec::text::clean_value(s)?)),
        _ => Err(ApiError::invalid(format!("{what} must be a string, a number or true/false."))),
    }
}

/// `{query, fetch}` paging options.
pub struct Fetch {
    pub limit: usize,
    pub ascending: bool,
    pub start_key: Option<Vec<u8>>,
}

pub fn fetch_opts(body: &Value, default_asc: bool) -> ApiResult<Fetch> {
    use base64::Engine;
    let f = body.get("fetch").cloned().unwrap_or(Value::Null);
    let limit = match f.get("limit") {
        None | Some(Value::Null) => 50,
        Some(v) => v.as_u64().filter(|n| (1..=1000).contains(n)).ok_or_else(|| ApiError::invalid("fetch.limit must be 1 to 1000."))? as usize,
    };
    let ascending = f.get("ascending").and_then(Value::as_bool).unwrap_or(default_asc);
    let start_key = match f.get("start_key") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s == "end" => None,
        Some(Value::String(s)) => Some(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(s)
                .map_err(|_| ApiError::invalid("fetch.start_key is not one DDBD gave out."))?,
        ),
        Some(_) => return Err(ApiError::invalid("fetch.start_key must be a string.")),
    };
    Ok(Fetch { limit, ascending, start_key })
}

pub fn page(list: Vec<Value>, cursor: Option<Vec<u8>>, end: bool) -> Value {
    use base64::Engine;
    let sk = match (&cursor, end) {
        (Some(c), false) => Value::String(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(c)),
        _ => Value::String("end".into()),
    };
    json!({"list": list, "start_key": sk, "end_of_list": end})
}
