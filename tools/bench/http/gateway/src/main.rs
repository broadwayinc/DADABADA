//! Benchmark only. Puts PostgreSQL or SQLite behind the same HTTP API shape DDBD serves, so all three
//! databases are measured through the same kind of request:
//!
//!   POST /v1/login       {"username","password"}            -> {"token"}
//!   POST /v1/getRecords  Authorization: Bearer <token>
//!                        {"query":{"collection","index":{"name":"price","value":X,"condition":">="},"tag"?},
//!                         "fetch":{"limit":N}}                -> {"end_of_list","list":[record...],"start_key"}
//!
//! Per request it does what DDBD's server does: hyper 1 on tokio with TCP_NODELAY, JSON body parsed with
//! serde_json, the token hashed with SHA-256 and looked up in an in-memory session table, the work done on a
//! blocking thread, and the answer built as serde_json values (full records, the same JSON DDBD returned
//! when they were saved) and serialized. SQLite runs inside this process, as RocksDB runs inside DDBD;
//! PostgreSQL is reached over its local socket, as any application server reaches it.
//!
//!   POST /v1/postRecords {"items":[{"data","config":{"collection":{"name","access_level"},"index":[price],"tags"}}]}
//!                                                           -> {"failed":[],"list":[record...]}
//!   POST /v1/postRecord  one item; with config.record_id it updates that record (data and price)
//!
//! Writes build the full record JSON DDBD answers with, store it, and answer with it. PostgreSQL gets one
//! multi-row INSERT per request (unnest of arrays), the fast way an app inserts a batch; SQLite runs prepared
//! inserts in one transaction. With "write", the durability matches DDBD's (a write survives a crash of
//! the server, not of the operating system): PostgreSQL synchronous_commit = off, SQLite WAL with
//! synchronous = NORMAL.
//!
//! MongoDB (reads only) keeps one document per record (../load_mongo.py): _id, collection, level, price, tags
//! and the record itself as a sub-document, with the same indexes as PostgreSQL. A query is one find with a
//! filter on collection, level, price (and tags), sorted by price and _id, limited to the page.
//!
//! Usage: sqlgw pg <port> <postgres uri> <table> [write]
//!        sqlgw sqlite <port> <sqlite file> <table> [write]
//!        sqlgw mongo <port> <mongodb uri> <collection holding the records>

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};

const POOL: usize = 8;

struct PgConn {
    client: postgres::Client,
    stmts: HashMap<bool, postgres::Statement>,
}

enum Store {
    Pg(Mutex<Vec<PgConn>>),
    Lite(Mutex<Vec<rusqlite::Connection>>),
    Mongo(mongodb::sync::Collection<mongodb::bson::Document>),
}

/// A record as a database hands it over: JSON text (PostgreSQL, SQLite), parsed once for the answer, or a
/// value already built from the database's own format (MongoDB's documents), used as it is.
enum Body {
    Text(String),
    Json(Value),
}

/// The answer to one query from MongoDB: (id, price, record), in (price, id) order.
fn run_mongo(coll: &mongodb::sync::Collection<mongodb::bson::Document>, q: &Q, level: i64) -> Result<Vec<(String, i64, Body)>, String> {
    use mongodb::bson::{doc, Bson};
    let mut filter = doc! {"collection": &q.collection, "level": {"$lte": level}, "price": {"$gte": q.value}};
    if let Some(t) = &q.tag {
        filter.insert("tags", t);
    }
    let opts = mongodb::options::FindOptions::builder()
        .sort(doc! {"price": 1, "_id": 1})
        .limit(q.limit)
        .projection(doc! {"price": 1, "rec": 1})
        .build();
    let mut out = vec![];
    for d in coll.find(filter, opts).map_err(|e| e.to_string())? {
        let d = d.map_err(|e| e.to_string())?;
        let id = d.get_str("_id").map_err(|e| e.to_string())?.to_string();
        let price = match d.get("price") {
            Some(Bson::Int32(n)) => *n as i64,
            Some(Bson::Int64(n)) => *n,
            _ => 0,
        };
        // relaxed extended JSON: numbers stay plain numbers, as in the record DDBD saved
        let rec = d.get("rec").cloned().unwrap_or(Bson::Null).into_relaxed_extjson();
        out.push((id, price, Body::Json(rec)));
    }
    Ok(out)
}

struct Session {
    level: i64,
    user_id: String,
    last: u64,
}

struct Gw {
    store: Store,
    table: String,
    sessions: Mutex<HashMap<String, Session>>,
    users: HashMap<String, (String, i64)>, // username -> (password, level)
}

/// The caller behind a token, as DDBD finds it: SHA-256 of the token in an in-memory table.
fn caller(gw: &Gw, token: &str) -> Option<(i64, String)> {
    let mut m = gw.sessions.lock().unwrap();
    let s = m.get_mut(&sha_hex(token))?;
    s.last = now_ms();
    Some((s.level, s.user_id.clone()))
}

fn new_id() -> String {
    use rand::Rng;
    const A: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut r = rand::thread_rng();
    (0..16).map(|_| A[r.gen_range(0..A.len())] as char).collect()
}

/// One record to save, checked.
struct Item {
    id: Option<String>, // set: update this record
    data: Value,
    collection: String,
    level: String,
    price: Option<i64>,
    tags: Vec<String>,
}

fn parse_item(v: &Value) -> Result<Item, String> {
    let data = v.get("data").cloned().unwrap_or(Value::Null);
    let cfg = v.get("config").and_then(Value::as_object).ok_or("config is required.")?;
    let id = cfg.get("record_id").and_then(Value::as_str).map(String::from);
    let (collection, level) = match cfg.get("collection") {
        Some(c) => (
            c.get("name").and_then(Value::as_str).ok_or("config.collection.name is required.")?.to_string(),
            c.get("access_level").and_then(Value::as_str).unwrap_or("00").to_string(),
        ),
        None if id.is_some() => (String::new(), String::new()),
        None => return Err("config.collection is required.".into()),
    };
    if level.len() > 2 || !level.chars().all(|c| c.is_ascii_digit()) {
        return Err("access_level must be two digits.".into());
    }
    let mut price = None;
    if let Some(ix) = cfg.get("index").and_then(Value::as_array) {
        for i in ix {
            if i.get("name").and_then(Value::as_str) != Some("price") {
                return Err("this gateway stores only the index price.".into());
            }
            price = Some(i.get("value").and_then(Value::as_i64).ok_or("price must be an integer.")?);
        }
    }
    let tags = cfg.get("tags").and_then(Value::as_array).map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect()).unwrap_or_default();
    Ok(Item { id, data, collection, level, price, tags })
}

/// The record as DDBD answers it.
fn record_json(id: &str, it: &Item, user_id: &str, uploaded: u64, updated: u64) -> Value {
    json!({
        "agent": null,
        "collection": {"access_level": it.level, "name": it.collection, "subscription": {"feed_referencing_records": false,
            "is_subscription_record": false, "notify_referencing_records": false, "notify_subscribers": false, "upload_to_feed": false}},
        "data": it.data,
        "index": it.price.map(|p| json!([{"name": "price", "value": p}])).unwrap_or(json!([])),
        "ip": "127.0.0.1",
        "readonly": false,
        "record_id": id,
        "reference": null,
        "referenced_count": 0,
        "source": {"allow_granted_to_grant_others": false, "can_remove_referencing_records": false, "only_granted_can_reference": false,
            "prevent_multiple_referencing": false, "referencing_index_restrictions": [], "referencing_limit": null},
        "tags": it.tags,
        "unique_id": null,
        "updated": updated,
        "uploaded": uploaded,
        "user_id": user_id,
    })
}

/// Saves new records in one transaction and answers them.
fn insert(gw: &Gw, items: &[Item], user_id: &str) -> Result<Vec<Value>, String> {
    let now = now_ms();
    let ids: Vec<String> = items.iter().map(|_| new_id()).collect();
    let recs: Vec<Value> = items.iter().zip(&ids).map(|(it, id)| record_json(id, it, user_id, now, now)).collect();
    let t = &gw.table;
    match &gw.store {
        Store::Pg(pool) => {
            let mut c = take(pool);
            let res = (|| -> Result<(), String> {
                let colls: Vec<&str> = items.iter().map(|i| i.collection.as_str()).collect();
                let lvls: Vec<i16> = items.iter().map(|i| i.level.parse::<i16>().unwrap_or(0)).collect();
                let prices: Vec<i64> = items.iter().map(|i| i.price.unwrap_or(0)).collect();
                let tags: Vec<String> = items.iter().map(|i| i.tags.join(",")).collect();
                let datas: Vec<String> = recs.iter().map(|r| r.to_string()).collect();
                let sql = format!(
                    "INSERT INTO {t} (id, collection, level, price, tags, data) \
                     SELECT id, c, l, p, string_to_array(tg, ','), d::jsonb \
                     FROM unnest($1::text[], $2::text[], $3::int2[], $4::int8[], $5::text[], $6::text[]) AS u(id, c, l, p, tg, d)"
                );
                c.client.execute(&sql, &[&ids, &colls, &lvls, &prices, &tags, &datas]).map_err(|e| e.to_string())?;
                Ok(())
            })();
            pool.lock().unwrap().push(c);
            res?;
        }
        Store::Mongo(_) => return Err("this benchmark does not write to MongoDB".into()),
        Store::Lite(pool) => {
            let mut c = take(pool);
            let res = (|| -> Result<(), String> {
                let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
                {
                    let mut ins = tx.prepare_cached(&format!("INSERT INTO {t} VALUES (?1, ?2, ?3, ?4, ?5)")).map_err(|e| e.to_string())?;
                    let mut tg = tx.prepare_cached("INSERT INTO record_tags VALUES (?1, ?2)").map_err(|e| e.to_string())?;
                    for ((it, id), r) in items.iter().zip(&ids).zip(&recs) {
                        let lvl: i64 = it.level.parse().unwrap_or(0);
                        ins.execute(rusqlite::params![id, it.collection, lvl, it.price.unwrap_or(0), r.to_string()]).map_err(|e| e.to_string())?;
                        for tag in &it.tags {
                            tg.execute(rusqlite::params![tag, id]).map_err(|e| e.to_string())?;
                        }
                    }
                }
                tx.commit().map_err(|e| e.to_string())
            })();
            pool.lock().unwrap().push(c);
            res?;
        }
    }
    Ok(recs)
}

/// Changes one record's data and price: read it, check the caller wrote it, write it back.
fn update(gw: &Gw, it: &Item, user_id: &str) -> Result<Value, String> {
    let id = it.id.as_deref().unwrap();
    let t = &gw.table;
    let merge = |old: &str| -> Result<(Value, i64), String> {
        let mut r: Value = serde_json::from_str(old).map_err(|e| e.to_string())?;
        if r["user_id"].as_str() != Some(user_id) {
            return Err(format!("FORBIDDEN: only the writer of record {id} may change it."));
        }
        r["data"] = it.data.clone();
        if let Some(p) = it.price {
            r["index"] = json!([{"name": "price", "value": p}]);
        }
        r["updated"] = json!(now_ms());
        let p = r["index"][0]["value"].as_i64().unwrap_or(0);
        Ok((r, p))
    };
    match &gw.store {
        Store::Pg(pool) => {
            let mut c = take(pool);
            let res = (|| -> Result<Value, String> {
                let mut tx = c.client.transaction().map_err(|e| e.to_string())?;
                let row = tx.query_opt(&format!("SELECT data::text FROM {t} WHERE id = $1 FOR UPDATE"), &[&id]).map_err(|e| e.to_string())?
                    .ok_or(format!("NOT_FOUND: record {id} does not exist."))?;
                let (r, p) = merge(&row.get::<_, String>(0))?;
                tx.execute(&format!("UPDATE {t} SET price = $2, data = ($3::text)::jsonb WHERE id = $1"), &[&id, &p, &r.to_string()]).map_err(|e| e.to_string())?;
                tx.commit().map_err(|e| e.to_string())?;
                Ok(r)
            })();
            pool.lock().unwrap().push(c);
            res
        }
        Store::Mongo(_) => Err("this benchmark does not write to MongoDB".into()),
        Store::Lite(pool) => {
            let mut c = take(pool);
            let res = (|| -> Result<Value, String> {
                let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
                let old: String = tx.query_row(&format!("SELECT data FROM {t} WHERE id = ?1"), [id], |r| r.get(0))
                    .map_err(|_| format!("NOT_FOUND: record {id} does not exist."))?;
                let (r, p) = merge(&old)?;
                tx.execute(&format!("UPDATE {t} SET price = ?2, data = ?3 WHERE id = ?1"), rusqlite::params![id, p, r.to_string()]).map_err(|e| e.to_string())?;
                tx.commit().map_err(|e| e.to_string())?;
                Ok(r)
            })();
            pool.lock().unwrap().push(c);
            res
        }
    }
}

fn post_records(gw: &Gw, token: &str, body: &Value, single: bool) -> Response<Full<Bytes>> {
    let Some((_, user_id)) = caller(gw, token) else {
        return err(401, "UNAUTHORIZED", "Your session ended or is unknown; sign in again.");
    };
    let raw: Vec<Value> = if single {
        vec![body.clone()]
    } else {
        match body.get("items").and_then(Value::as_array) {
            Some(a) => a.clone(),
            None => return err(400, "INVALID_REQUEST", "items must be a list."),
        }
    };
    let mut items = vec![];
    for v in &raw {
        match parse_item(v) {
            Ok(i) => items.push(i),
            Err(e) => return err(400, "INVALID_REQUEST", &e),
        }
    }
    if single && items[0].id.is_some() {
        return match update(gw, &items[0], &user_id) {
            Ok(r) => json_response(200, &r),
            Err(e) => err(if e.starts_with("NOT_FOUND") { 404 } else if e.starts_with("FORBIDDEN") { 403 } else { 500 }, "WRITE", &e),
        };
    }
    match insert(gw, &items, &user_id) {
        Ok(recs) if single => json_response(200, &recs[0]),
        Ok(recs) => json_response(200, &json!({"failed": [], "list": recs})),
        Err(e) => err(500, "INTERNAL", &e),
    }
}

/// A connection from the pool; waits while all are busy (SQLite writes have a pool of one).
fn take<T>(pool: &Mutex<Vec<T>>) -> T {
    loop {
        if let Some(c) = pool.lock().unwrap().pop() {
            return c;
        }
        std::thread::sleep(std::time::Duration::from_micros(50));
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}

fn sha_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

fn json_response(status: u16, v: &Value) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header("cache-control", "no-store")
        .body(Full::new(Bytes::from(serde_json::to_vec(v).unwrap())))
        .unwrap()
}

fn err(status: u16, code: &str, msg: &str) -> Response<Full<Bytes>> {
    json_response(status, &json!({"error": {"code": code, "message": msg}}))
}

struct Q {
    collection: String,
    value: i64,
    tag: Option<String>,
    limit: i64,
}

fn parse_query(body: &Value) -> Result<Q, String> {
    let q = body.get("query").and_then(Value::as_object).ok_or("query must be an object.")?;
    let collection = q.get("collection").and_then(Value::as_str).ok_or("query.collection is required.")?.to_string();
    let ix = q.get("index").and_then(Value::as_object).ok_or("query.index is required.")?;
    if ix.get("name").and_then(Value::as_str) != Some("price") || ix.get("condition").and_then(Value::as_str) != Some(">=") {
        return Err("this gateway answers only index price with condition >=.".into());
    }
    let value = ix.get("value").and_then(Value::as_i64).ok_or("query.index.value must be an integer.")?;
    let tag = q.get("tag").and_then(Value::as_str).map(String::from);
    let limit = body.pointer("/fetch/limit").and_then(Value::as_i64).unwrap_or(50).clamp(1, 1000);
    Ok(Q { collection, value, tag, limit })
}

fn pg_sql(t: &str, tag: bool) -> String {
    let tag_cond = if tag { " AND tags @> ARRAY[$5::text]" } else { "" };
    format!(
        "SELECT id, price, data::text FROM {t} WHERE collection = $1 AND price >= $2 AND level <= $3{tag_cond} \
         ORDER BY price, id LIMIT $4"
    )
}

fn lite_sql(t: &str, tag: bool) -> String {
    let tag_cond = if tag { " AND id IN (SELECT id FROM record_tags WHERE tag = ?5)" } else { "" };
    format!(
        "SELECT id, price, data FROM {t} WHERE collection = ?1 AND price >= ?2 AND level <= ?3{tag_cond} \
         ORDER BY price, id LIMIT ?4"
    )
}

/// The rows of one answer: (id, price, record JSON text).
fn run(gw: &Gw, q: &Q, level: i64) -> Result<Vec<(String, i64, Body)>, String> {
    let has_tag = q.tag.is_some();
    match &gw.store {
        Store::Pg(pool) => {
            let mut c = take(pool);
            let res = (|| {
                if !c.stmts.contains_key(&has_tag) {
                    let s = c.client.prepare(&pg_sql(&gw.table, has_tag)).map_err(|e| e.to_string())?;
                    c.stmts.insert(has_tag, s);
                }
                let st = c.stmts[&has_tag].clone();
                let lvl = level as i16;
                let rows = match &q.tag {
                    Some(t) => c.client.query(&st, &[&q.collection, &q.value, &lvl, &q.limit, t]),
                    None => c.client.query(&st, &[&q.collection, &q.value, &lvl, &q.limit]),
                }
                .map_err(|e| e.to_string())?;
                Ok(rows.iter().map(|r| (r.get::<_, String>(0), r.get::<_, i64>(1), Body::Text(r.get::<_, String>(2)))).collect())
            })();
            pool.lock().unwrap().push(c);
            res
        }
        Store::Mongo(coll) => run_mongo(coll, q, level),
        Store::Lite(pool) => {
            let c = take(pool);
            let res = (|| {
                let mut st = c.prepare_cached(&lite_sql(&gw.table, has_tag)).map_err(|e| e.to_string())?;
                let map = |r: &rusqlite::Row| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, Body::Text(r.get::<_, String>(2)?)));
                let rows: Result<Vec<_>, _> = match &q.tag {
                    Some(t) => st.query_map(rusqlite::params![q.collection, q.value, level, q.limit, t], map),
                    None => st.query_map(rusqlite::params![q.collection, q.value, level, q.limit], map),
                }
                .map_err(|e| e.to_string())?
                .collect();
                rows.map_err(|e| e.to_string())
            })();
            pool.lock().unwrap().push(c);
            res
        }
    }
}

fn get_records(gw: &Gw, token: &str, body: &Value) -> Response<Full<Bytes>> {
    let Some((level, _)) = caller(gw, token) else {
        return err(401, "UNAUTHORIZED", "Your session ended or is unknown; sign in again.");
    };
    let q = match parse_query(body) {
        Ok(q) => q,
        Err(e) => return err(400, "INVALID_REQUEST", &e),
    };
    let rows = match run(gw, &q, level) {
        Ok(r) => r,
        Err(e) => return err(500, "INTERNAL", &e),
    };
    let end = (rows.len() as i64) < q.limit;
    let start_key = if end { "end".to_string() } else { rows.last().map(|(id, p, _)| format!("{p}:{id}")).unwrap() };
    let list: Vec<Value> = rows
        .into_iter()
        .map(|(_, _, d)| match d {
            Body::Text(t) => serde_json::from_str(&t).unwrap_or(Value::Null),
            Body::Json(v) => v,
        })
        .collect();
    json_response(200, &json!({"end_of_list": end, "list": list, "start_key": start_key}))
}

fn login(gw: &Gw, body: &Value) -> Response<Full<Bytes>> {
    let u = body["username"].as_str().unwrap_or("");
    let p = body["password"].as_str().unwrap_or("");
    match gw.users.get(u) {
        Some((pw, level)) if pw == p => {
            let tok: String = {
                use rand::Rng;
                let b: [u8; 24] = rand::thread_rng().gen();
                hex::encode(b)
            };
            gw.sessions.lock().unwrap().insert(sha_hex(&tok), Session { level: *level, user_id: format!("{u}-id"), last: now_ms() });
            json_response(200, &json!({"token": tok}))
        }
        _ => err(401, "UNAUTHORIZED", "Wrong username or password."),
    }
}

async fn handle(gw: Arc<Gw>, req: Request<Incoming>) -> Response<Full<Bytes>> {
    if req.method() != hyper::Method::POST {
        return err(405, "METHOD", "Use POST for /v1/<method>.");
    }
    let path = req.uri().path().to_string();
    let token = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(|t| t.trim().to_string());
    let bytes = match req.into_body().collect().await {
        Ok(b) => b.to_bytes(),
        Err(e) => return err(400, "INVALID_REQUEST", &format!("Could not read the request body: {e}")),
    };
    let body: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => return err(400, "INVALID_REQUEST", &format!("The body is not valid JSON: {e}")),
    };
    match path.as_str() {
        "/v1/login" => tokio::task::spawn_blocking(move || login(&gw, &body)).await.unwrap(),
        "/v1/getRecords" => {
            let Some(token) = token else {
                return err(401, "UNAUTHORIZED", "Sign in first: send Authorization: Bearer <session token>.");
            };
            tokio::task::spawn_blocking(move || get_records(&gw, &token, &body)).await.unwrap()
        }
        "/v1/postRecords" | "/v1/postRecord" => {
            let Some(token) = token else {
                return err(401, "UNAUTHORIZED", "Sign in first: send Authorization: Bearer <session token>.");
            };
            let single = path == "/v1/postRecord";
            tokio::task::spawn_blocking(move || post_records(&gw, &token, &body, single)).await.unwrap()
        }
        _ => err(404, "NOT_FOUND", &format!("Nothing at {path}.")),
    }
}

#[tokio::main]
async fn main() {
    let a: Vec<String> = std::env::args().collect();
    let write = a.len() == 6 && a[5] == "write";
    if a.len() != 5 && !write {
        eprintln!("usage: sqlgw pg|sqlite|mongo <port> <postgres uri | sqlite file | mongodb uri> <table> [write]");
        std::process::exit(2);
    }
    let (engine, port, target, table) = (a[1].as_str(), a[2].parse::<u16>().unwrap(), a[3].clone(), a[4].clone());
    let store = match engine {
        "pg" => {
            let mut v = Vec::new();
            for _ in 0..POOL {
                let t2 = target.clone();
                // the sync client runs its own small runtime, so it is made off the async threads
                let c = tokio::task::spawn_blocking(move || postgres::Client::connect(&t2, postgres::NoTls)).await.unwrap();
                let mut client = c.expect("connect to PostgreSQL");
                if write {
                    // like DDBD: a commit survives a crash of the database, not of the operating system
                    client = tokio::task::spawn_blocking(move || {
                        client.batch_execute("SET synchronous_commit = off").expect("set synchronous_commit");
                        client
                    })
                    .await
                    .unwrap();
                }
                v.push(PgConn { client, stmts: HashMap::new() });
            }
            Store::Pg(Mutex::new(v))
        }
        "sqlite" => {
            let mut v = Vec::new();
            // SQLite takes one writer at a time: with several connections writing, one of them always holds a
            // transaction, the WAL can never be folded back into the database and grows without end. So writes
            // get one connection; the other requests wait for it, as in any app that writes to SQLite.
            for _ in 0..(if write { 1 } else { POOL }) {
                let flags = if write { rusqlite::OpenFlags::default() } else { rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY };
                let c = rusqlite::Connection::open_with_flags(&target, flags).expect("open SQLite");
                // the same settings as the earlier in-process run: the whole file stays in memory
                c.execute_batch("PRAGMA cache_size = -2000000; PRAGMA mmap_size = 4000000000;").unwrap();
                if write {
                    // like DDBD: a commit survives a crash of the server, not of the operating system
                    c.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA busy_timeout = 30000;").unwrap();
                }
                v.push(c);
            }
            Store::Lite(Mutex::new(v))
        }
        "mongo" => {
            let t2 = target.clone();
            let tb = table.clone();
            // the driver keeps its own pool of connections; the sync API runs it on its own runtime
            let coll = tokio::task::spawn_blocking(move || {
                let mut o = mongodb::options::ClientOptions::parse(&t2).expect("MongoDB uri");
                o.max_pool_size = Some(POOL as u32);
                let c = mongodb::sync::Client::with_options(o).expect("MongoDB client");
                c.database("bench").collection::<mongodb::bson::Document>(&tb)
            })
            .await
            .unwrap();
            Store::Mongo(coll)
        }
        _ => panic!("engine must be pg, sqlite or mongo"),
    };
    let mut users = HashMap::new();
    for l in 0..4i64 {
        users.insert(format!("u{l:02}"), (format!("u{l:02}-pass"), l));
    }
    users.insert("loader".to_string(), ("loader-pass".to_string(), 99));
    let gw = Arc::new(Gw { store, table, sessions: Mutex::new(HashMap::new()), users });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    let ver = match engine {
        "sqlite" => format!("SQLite {}", rusqlite::version()),
        "mongo" => "MongoDB".to_string(),
        _ => "PostgreSQL".to_string(),
    };
    println!("Ready. sqlgw ({ver}) on 127.0.0.1:{port}");
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(s) => s,
            Err(_) => continue,
        };
        let _ = stream.set_nodelay(true);
        let gw = gw.clone();
        tokio::spawn(async move {
            let svc = hyper::service::service_fn(move |req| {
                let g = gw.clone();
                async move { Ok::<_, Infallible>(handle(g, req).await) }
            });
            let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), svc).await;
        });
    }
}
