//! `GET /v1/listen`: Server-Sent Events with resume by Last-Event-ID.

use super::http::Body;
use crate::engine::access::Caller;
use crate::engine::worker::ntf_key;
use crate::engine::{Db, Notice};
use crate::store::{CF_META, CF_NTF};
use bytes::Bytes;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::Frame;
use hyper::Response;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};

fn event(n: &Notice) -> Bytes {
    Bytes::from(format!("id: {:016x}\nevent: notify\ndata: {}\n\n", n.seq, n.data))
}

/// An agent key hears only about records it could read itself (collections, max_level, private).
fn allowed(db: &Db, c: &Caller, n: &Notice) -> bool {
    if c.agent.is_none() {
        return true;
    }
    let Some(id) = n.data.get("record_id").and_then(|v| v.as_str()) else { return false };
    match db.load_meta(id) {
        Ok(Some(m)) => crate::engine::access::can_read(db, c, id, &m, None).unwrap_or(false),
        _ => false,
    }
}

/// The stream ends when the server stops (`stop` becomes true), so a stop does not wait on listeners.
pub fn stream(db: Arc<Db>, c: Caller, last: Option<u64>, mut stop: tokio::sync::watch::Receiver<bool>) -> Response<Body> {
    let user = c.user_id.clone();
    let mut rx = {
        let mut ls = db.listeners.lock().unwrap();
        ls.entry(user.clone()).or_insert_with(|| broadcast::channel(256).0).subscribe()
    };
    let (tx, out) = mpsc::channel::<Bytes>(64);
    tokio::spawn(async move {
        let mut sent = last.unwrap_or(0);
        if let Some(l) = last {
            let mut wk = b"w".to_vec();
            wk.extend_from_slice(user.as_bytes());
            let mark = db.store.get(CF_META, &wk).ok().flatten().map(|v| u64::from_be_bytes(v[..8].try_into().unwrap())).unwrap_or(0);
            if mark > l && tx.send(Bytes::from("event: gap\ndata: {\"message\":\"Some notifications expired before you reconnected.\"}\n\n")).await.is_err() {
                return;
            }
            let lo = ntf_key(&user, l + 1);
            let hi = ntf_key(&user, u64::MAX);
            let mut stored = vec![];
            let _ = db.store.scan(CF_NTF, &lo, &hi, true, |k, v| {
                let seq = u64::from_be_bytes(k[k.len() - 8..].try_into().unwrap());
                if let Ok(data) = serde_json::from_slice(v) {
                    stored.push(Notice { seq, data });
                }
                true
            });
            for n in stored {
                sent = sent.max(n.seq);
                if !allowed(&db, &c, &n) {
                    continue;
                }
                if tx.send(event(&n)).await.is_err() {
                    return;
                }
            }
        }
        if tx.send(Bytes::from(": connected\n\n")).await.is_err() {
            return;
        }
        let mut ping = tokio::time::interval(Duration::from_secs(30));
        ping.tick().await;
        loop {
            tokio::select! {
                r = rx.recv() => match r {
                    Ok(n) if n.seq > sent => {
                        sent = n.seq;
                        if !allowed(&db, &c, &n) { continue; }
                        if tx.send(event(&n)).await.is_err() { return; }
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        if tx.send(Bytes::from("event: gap\ndata: {\"message\":\"Too slow; some notifications were skipped. Reconnect with Last-Event-ID to catch up.\"}\n\n")).await.is_err() { return; }
                    }
                    Err(_) => return,
                },
                _ = ping.tick() => {
                    if tx.send(Bytes::from(": ping\n\n")).await.is_err() { return; }
                }
                _ = super::http::stopped(&mut stop) => return,
            }
        }
    });
    let s = futures_util::stream::unfold(out, |mut out| async move {
        out.recv().await.map(|b| (Ok::<_, Infallible>(Frame::data(b)), out))
    });
    Response::builder()
        .status(200)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-store")
        .header("x-accel-buffering", "no")
        .body(StreamBody::new(s).boxed())
        .unwrap()
}
