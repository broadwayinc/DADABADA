//! The database engine: records, queries, users, background work.

pub mod access;
pub mod agents;
pub mod catalog;
pub mod events;
pub mod json;
pub mod query;
pub mod subs;
pub mod transfer;
pub mod users;
pub mod views;
pub mod worker;
pub mod write;

use crate::codec::body::{self, Meta};
use crate::config::Config;
use crate::error::{ApiError, ApiResult};
use crate::store::{Batch, Store, CF_META, CF_NTF, CF_REC};
use events::Event;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, RwLock};

/// Record locks are striped by a hash of the name. Many stripes, so that the up to BULK_MAX stripes a
/// postRecords request holds while it saves a group of records (see write::post_records) are a small share.
const LOCK_STRIPES: usize = 65536;

/// A live notification for SSE listeners.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notice {
    pub seq: u64,
    pub data: serde_json::Value,
}

#[derive(Default)]
pub struct Stats {
    pub requests: AtomicU64,
    pub connections: AtomicU64,
}

pub struct Db {
    pub store: Store,
    config: RwLock<Config>,
    pub data_dir: PathBuf,
    locks: Vec<Mutex<()>>,
    ntf_seq: AtomicU64,
    pub wake: (Mutex<bool>, Condvar),
    pub secret: RwLock<[u8; 32]>,
    pub sessions: Mutex<HashMap<String, users::Session>>,
    pub listeners: Mutex<HashMap<String, tokio::sync::broadcast::Sender<Notice>>>,
    pub throttle: Mutex<users::Throttle>,
    pub stats: Stats,
    pub stopping: AtomicBool,
    /// Set by the `shutdown` admin command; the server then stops the same way as on Ctrl-C.
    pub shutdown: tokio::sync::Notify,
    /// With `shutdown`: start again in the same process (same pid, same terminal) after stopping.
    pub restart: AtomicBool,
    /// Web admin sessions: sha256(token) -> last use (ms). Memory only.
    pub admin_sessions: Mutex<HashMap<String, u64>>,
    /// Requests that write records, running now (see `writing` and `busy`).
    writers: AtomicUsize,
}

/// Counts one request that writes records for as long as it is alive.
pub struct Writing<'a>(&'a AtomicUsize);

impl Drop for Writing<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Db {
    pub fn open(data_dir: &Path, cfg: Config) -> Result<Db, String> {
        let store = Store::open(&data_dir.join("db"), &cfg)?;
        let secret = match store.get(CF_META, b"secret").map_err(|e| e.message)? {
            Some(s) if s.len() == 32 => s.try_into().unwrap(),
            _ => {
                let s: [u8; 32] = rand::random();
                store.put(CF_META, b"secret", &s).map_err(|e| e.message)?;
                s
            }
        };
        let ntf_saved = store
            .get(CF_META, b"ntf_seq")
            .map_err(|e| e.message)?
            .map(|v| u64::from_be_bytes(v[..8].try_into().unwrap()))
            .unwrap_or(0);
        let db = Db {
            store,
            config: RwLock::new(cfg),
            data_dir: data_dir.to_path_buf(),
            locks: (0..LOCK_STRIPES).map(|_| Mutex::new(())).collect(),
            ntf_seq: AtomicU64::new(ntf_saved),
            wake: (Mutex::new(false), Condvar::new()),
            secret: RwLock::new(secret),
            sessions: Mutex::new(HashMap::new()),
            listeners: Mutex::new(HashMap::new()),
            throttle: Mutex::new(users::Throttle::default()),
            stats: Stats::default(),
            stopping: AtomicBool::new(false),
            shutdown: tokio::sync::Notify::new(),
            restart: AtomicBool::new(false),
            admin_sessions: Mutex::new(HashMap::new()),
            writers: AtomicUsize::new(0),
        };
        // the ntf sequence must also be above every stored notification: a user's notifications are in
        // sequence order, so the last key of each user is enough (one seek per user, not a full read)
        let mut max_ntf = db.ntf_seq.load(Ordering::SeqCst);
        {
            let mut it = db.store.db.raw_iterator_cf(db.store.cf(CF_NTF));
            it.seek_to_first();
            while it.valid() {
                let k = it.key().unwrap();
                if k.len() < 9 {
                    it.next();
                    continue;
                }
                let end = crate::codec::keys::prefix_end(&k[..k.len() - 8]);
                it.seek_for_prev(&end);
                if let Some(last) = it.key().filter(|l| l.len() >= 8) {
                    max_ntf = max_ntf.max(u64::from_be_bytes(last[last.len() - 8..].try_into().unwrap()));
                }
                it.seek(&end);
            }
            it.status().map_err(|e| e.to_string())?;
        }
        db.ntf_seq.store(max_ntf, Ordering::SeqCst);
        if db.store.get(CF_META, b"loose_version").map_err(|e| e.message)?.is_none() {
            db.store
                .put(CF_META, b"loose_version", &crate::codec::loose::LOOSE_VERSION.to_be_bytes())
                .map_err(|e| e.message)?;
        }
        users::load_sessions(&db).map_err(|e| e.message)?;
        // at every start, not once: a folder written meanwhile by a version without level marks is covered too
        catalog::build_level_marks(&db).map_err(|e| e.message)?;
        if db.store.get(CF_META, b"user_index_v1").map_err(|e| e.message)?.is_none() {
            users::reindex_users(&db).map_err(|e| e.message)?;
            db.store.put(CF_META, b"user_index_v1", b"1").map_err(|e| e.message)?;
        }
        Ok(db)
    }

    /// Locks several names (record ids, `u:<unique id>`, `user:<id>`) in a fixed order.
    pub fn lock_many<'a>(&'a self, names: &[&str]) -> Vec<MutexGuard<'a, ()>> {
        let mut idx: Vec<usize> = names.iter().filter(|n| !n.is_empty()).map(|n| self.stripe(n)).collect();
        idx.sort_unstable();
        idx.dedup();
        idx.into_iter().map(|i| self.lock_stripe(i)).collect()
    }

    /// The lock stripe of a name (a record id, a reference target, a unique id).
    pub fn stripe(&self, name: &str) -> usize {
        let mut h: u64 = 1469598103934665603;
        for b in name.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(1099511628211);
        }
        (h % LOCK_STRIPES as u64) as usize
    }

    /// Takes one stripe, waiting for it. Whoever waits here must hold no other stripe, or take them in order
    /// (as lock_many does), so two writers can never wait for each other.
    pub fn lock_stripe(&self, i: usize) -> MutexGuard<'_, ()> {
        self.locks[i].lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Takes one stripe if it is free now, without waiting.
    pub fn try_lock_stripe(&self, i: usize) -> Option<MutexGuard<'_, ()>> {
        match self.locks[i].try_lock() {
            Ok(g) => Some(g),
            Err(std::sync::TryLockError::Poisoned(e)) => Some(e.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    }

    /// Counts the calling request as one that writes records, until the returned value is dropped.
    pub fn writing(&self) -> Writing<'_> {
        self.writers.fetch_add(1, Ordering::SeqCst);
        Writing(&self.writers)
    }

    /// True when more than one request is writing records right now: a request saving many records then
    /// saves them one at a time, so the others are not kept waiting.
    pub fn busy(&self) -> bool {
        self.writers.load(Ordering::SeqCst) > 1
    }

    /// Adds an event to a batch; the worker applies it after the batch commits.
    pub fn push_event(&self, b: &mut Batch, ev: &Event) {
        b.add_event(&rmp_serde::to_vec_named(ev).expect("event encodes"));
    }

    pub fn wake_worker(&self) {
        let (m, c) = &self.wake;
        *m.lock().unwrap() = true;
        c.notify_one();
    }

    pub fn next_ntf_seq(&self) -> u64 {
        let s = self.ntf_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = self.store.put(CF_META, b"ntf_seq", &s.to_be_bytes());
        s
    }

    pub fn outbox_depth(&self) -> u64 {
        self.store.outbox.len()
    }

    pub fn load_record(&self, id: &str) -> ApiResult<Option<(Meta, Vec<u8>)>> {
        match self.store.get(CF_REC, id.as_bytes())? {
            None => Ok(None),
            Some(bytes) => {
                let (m, d) = body::decode(&bytes).ok_or_else(|| ApiError::internal(format!("record {id} is damaged")))?;
                Ok(Some((m, d.to_vec())))
            }
        }
    }

    pub fn load_meta(&self, id: &str) -> ApiResult<Option<Meta>> {
        Ok(self.load_record(id)?.map(|(m, _)| m))
    }

    /// The settings in force (limits and keep times can change while running).
    pub fn cfg(&self) -> Config {
        self.config.read().unwrap().clone()
    }

    pub fn set_cfg(&self, c: Config) {
        *self.config.write().unwrap() = c;
    }

    pub fn secret(&self) -> [u8; 32] {
        *self.secret.read().unwrap()
    }
}

pub fn now() -> u64 {
    crate::codec::ids::now_ms()
}
