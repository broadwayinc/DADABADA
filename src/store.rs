//! RocksDB storage. Everything else goes through these helpers.

use crate::config::Config;
use crate::error::{ApiError, ApiResult};
use rocksdb::{
    BlockBasedOptions, Cache, ColumnFamily, ColumnFamilyDescriptor, DBCompressionType, Options, ReadOptions, WriteBatch,
    DB,
};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

pub const CF_REC: &str = "rec";
pub const CF_IX: &str = "ix";
pub const CF_UNQ: &str = "unq";
pub const CF_USR: &str = "usr";
pub const CF_KEY: &str = "key";
pub const CF_ACC: &str = "acc";
pub const CF_SUB: &str = "sub";
pub const CF_FEED: &str = "feed";
pub const CF_NTF: &str = "ntf";
pub const CF_CAT: &str = "cat";
pub const CF_EVT: &str = "evt";
pub const CF_META: &str = "meta";

pub const ALL_CFS: [&str; 12] = [
    CF_REC, CF_IX, CF_UNQ, CF_USR, CF_KEY, CF_ACC, CF_SUB, CF_FEED, CF_NTF, CF_CAT, CF_EVT, CF_META,
];

pub struct Store {
    pub db: DB,
    _cache: Cache,
    pub outbox: Outbox,
}

/// Bookkeeping for the outbox (CF_EVT), whose keys are sequence numbers.
///
/// A number is handed out when an event is added to a batch, but batches commit in any order: event 101
/// can be stored while 100 is still on its way. `in_flight` holds the numbers handed out and not yet
/// committed or dropped, so the worker knows how far it may move its read position without passing an
/// event that has yet to appear. `stored` counts the events in the outbox without scanning it.
pub struct Outbox {
    state: Mutex<OutboxState>,
    stored: AtomicI64,
}

struct OutboxState {
    last: u64,
    in_flight: BTreeSet<u64>,
}

impl Outbox {
    fn new(last: u64, stored: u64) -> Outbox {
        Outbox { state: Mutex::new(OutboxState { last, in_flight: BTreeSet::new() }), stored: AtomicI64::new(stored as i64) }
    }

    /// The next sequence number, marked as in flight until its batch commits or is dropped.
    fn take(&self) -> u64 {
        let mut st = self.state.lock().unwrap();
        st.last += 1;
        let n = st.last;
        st.in_flight.insert(n);
        n
    }

    fn settle(&self, seqs: &[u64], stored: bool) {
        let mut st = self.state.lock().unwrap();
        for s in seqs {
            st.in_flight.remove(s);
        }
        if stored {
            self.stored.fetch_add(seqs.len() as i64, Ordering::SeqCst);
        }
    }

    /// Every event numbered below this is stored (or was never written): the lowest number in flight,
    /// or the next one to be handed out.
    pub fn settled_below(&self) -> u64 {
        let st = self.state.lock().unwrap();
        st.in_flight.first().copied().unwrap_or(st.last + 1)
    }

    /// How many events wait in the outbox.
    pub fn len(&self) -> u64 {
        self.stored.load(Ordering::SeqCst).max(0) as u64
    }
}

impl Store {
    pub fn open(path: &Path, cfg: &Config) -> Result<Store, String> {
        let cache = Cache::new_lru_cache((cfg.rocksdb.block_cache_mb.max(8) * 1024 * 1024) as usize);
        let mut bbo = BlockBasedOptions::default();
        bbo.set_block_cache(&cache);
        bbo.set_bloom_filter(10.0, false);
        bbo.set_cache_index_and_filter_blocks(true);

        let base = |blob: bool| {
            let mut o = Options::default();
            o.set_block_based_table_factory(&bbo);
            o.set_compression_type(DBCompressionType::Lz4);
            o.set_bottommost_compression_type(DBCompressionType::Zstd);
            if blob {
                // Large record bodies go to blob files; only a pointer is compacted.
                o.set_enable_blob_files(true);
                o.set_min_blob_size(cfg.rocksdb.blob_min_kb.max(1) * 1024);
                o.set_blob_compression_type(DBCompressionType::Lz4);
                o.set_enable_blob_gc(true);
            }
            o
        };

        let mut opts = base(false);
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);
        opts.increase_parallelism(std::thread::available_parallelism().map(|n| n.get() as i32).unwrap_or(2));
        opts.set_keep_log_file_num(5);
        // A write-ahead log file is kept until every column family that wrote to it is flushed; small, rarely
        // written ones (users, settings) would keep old files for a long time and make every start replay
        // them. Past this size the oldest ones are flushed, which bounds the replay after a crash.
        opts.set_max_total_wal_size(256 * 1024 * 1024);

        let cfs: Vec<ColumnFamilyDescriptor> = ALL_CFS
            .iter()
            .map(|n| ColumnFamilyDescriptor::new(*n, base(*n == CF_REC)))
            .collect();
        let db = DB::open_cf_descriptors(&opts, path, cfs).map_err(|e| {
            let msg = e.to_string();
            if msg.contains("lock") {
                format!("The database at {} is already open in another DDBD process ({msg}).", path.display())
            } else {
                format!("Could not open the database at {}: {msg}", path.display())
            }
        })?;
        // The outbox is small when the worker keeps up. Compacting it here drops the deletion markers a
        // stop or a crash left behind, so the first pass of the worker does not have to step over them.
        let evt = db.cf_handle(CF_EVT).expect("column family exists");
        db.compact_range_cf(evt, None::<&[u8]>, None::<&[u8]>);
        let (mut last, mut stored) = (0u64, 0u64);
        let mut it = db.raw_iterator_cf(evt);
        it.seek_to_first();
        while it.valid() {
            let k = it.key().unwrap();
            if k.len() == 8 {
                last = last.max(u64::from_be_bytes(k.try_into().unwrap()));
            }
            stored += 1;
            it.next();
        }
        it.status().map_err(|e| format!("Could not read the outbox at {}: {e}", path.display()))?;
        drop(it);
        Ok(Store { db, _cache: cache, outbox: Outbox::new(last, stored) })
    }

    /// Writes every column family's memory table to disk, so the next start has no log to replay.
    pub fn flush_all(&self) -> ApiResult<()> {
        let cfs: Vec<&ColumnFamily> = ALL_CFS.iter().map(|n| self.cf(n)).collect();
        Ok(self.db.flush_cfs_opt(&cfs, &rocksdb::FlushOptions::default())?)
    }

    pub fn cf(&self, name: &str) -> &ColumnFamily {
        self.db.cf_handle(name).expect("column family exists")
    }

    pub fn get(&self, cf: &str, key: &[u8]) -> ApiResult<Option<Vec<u8>>> {
        Ok(self.db.get_cf(self.cf(cf), key)?)
    }

    pub fn exists(&self, cf: &str, key: &[u8]) -> ApiResult<bool> {
        Ok(self.db.get_pinned_cf(self.cf(cf), key)?.is_some())
    }

    pub fn put(&self, cf: &str, key: &[u8], val: &[u8]) -> ApiResult<()> {
        Ok(self.db.put_cf(self.cf(cf), key, val)?)
    }

    pub fn delete(&self, cf: &str, key: &[u8]) -> ApiResult<()> {
        Ok(self.db.delete_cf(self.cf(cf), key)?)
    }

    pub fn write(&self, b: WriteBatch) -> ApiResult<()> {
        Ok(self.db.write(b)?)
    }

    pub fn multi_get(&self, cf: &str, keys: &[Vec<u8>]) -> ApiResult<Vec<Option<Vec<u8>>>> {
        let h = self.cf(cf);
        let res = self.db.multi_get_cf(keys.iter().map(|k| (h, k.as_slice())));
        res.into_iter().map(|r| r.map_err(ApiError::from)).collect()
    }

    /// Visits keys in `[lo, hi)` in order. `f` returns false to stop.
    pub fn scan(
        &self,
        cf: &str,
        lo: &[u8],
        hi: &[u8],
        asc: bool,
        mut f: impl FnMut(&[u8], &[u8]) -> bool,
    ) -> ApiResult<()> {
        let mut ro = ReadOptions::default();
        ro.set_iterate_lower_bound(lo.to_vec());
        ro.set_iterate_upper_bound(hi.to_vec());
        let mut it = self.db.raw_iterator_cf_opt(self.cf(cf), ro);
        if asc {
            it.seek_to_first();
        } else {
            it.seek_to_last();
        }
        while it.valid() {
            let (k, v) = (it.key().unwrap(), it.value().unwrap());
            if !f(k, v) {
                break;
            }
            if asc {
                it.next();
            } else {
                it.prev();
            }
        }
        it.status()?;
        Ok(())
    }

    pub fn scan_prefix(&self, cf: &str, prefix: &[u8], f: impl FnMut(&[u8], &[u8]) -> bool) -> ApiResult<()> {
        let hi = crate::codec::keys::prefix_end(prefix);
        self.scan(cf, prefix, &hi, true, f)
    }

    /// Whether any key starts with `prefix`; reads at most one key.
    pub fn has_prefix(&self, cf: &str, prefix: &[u8]) -> ApiResult<bool> {
        let mut found = false;
        self.scan_prefix(cf, prefix, |_, _| {
            found = true;
            false
        })?;
        Ok(found)
    }

    pub fn count_prefix(&self, cf: &str, prefix: &[u8]) -> ApiResult<u64> {
        let mut n = 0;
        self.scan_prefix(cf, prefix, |_, _| {
            n += 1;
            true
        })?;
        Ok(n)
    }
}

/// A write batch that knows the store, so callers write `b.put(CF_IX, k, v)`.
pub struct Batch<'a> {
    pub store: &'a Store,
    pub wb: WriteBatch,
    /// outbox events this batch adds (sequence numbers in flight) and removes
    evt_added: Vec<u64>,
    evt_removed: i64,
}

impl<'a> Batch<'a> {
    pub fn new(store: &'a Store) -> Self {
        Batch { store, wb: WriteBatch::default(), evt_added: Vec::new(), evt_removed: 0 }
    }
    /// Adds an event to the outbox under the next sequence number.
    pub fn add_event(&mut self, v: &[u8]) {
        let seq = self.store.outbox.take();
        self.wb.put_cf(self.store.cf(CF_EVT), seq.to_be_bytes(), v);
        self.evt_added.push(seq);
    }
    /// Removes a stored event from the outbox.
    pub fn remove_event(&mut self, k: &[u8]) {
        self.wb.delete_cf(self.store.cf(CF_EVT), k);
        self.evt_removed += 1;
    }
    pub fn put(&mut self, cf: &str, k: &[u8], v: &[u8]) {
        self.wb.put_cf(self.store.cf(cf), k, v);
    }
    pub fn delete(&mut self, cf: &str, k: &[u8]) {
        self.wb.delete_cf(self.store.cf(cf), k);
    }
    pub fn delete_range(&mut self, cf: &str, lo: &[u8], hi: &[u8]) {
        self.wb.delete_range_cf(self.store.cf(cf), lo, hi);
    }
    pub fn commit(mut self) -> ApiResult<()> {
        let r = self.store.write(std::mem::take(&mut self.wb));
        let added = std::mem::take(&mut self.evt_added);
        self.store.outbox.settle(&added, r.is_ok());
        if r.is_ok() {
            self.store.outbox.stored.fetch_sub(self.evt_removed, Ordering::SeqCst);
        }
        r
    }
}

impl Drop for Batch<'_> {
    /// A batch dropped without commit never stored its events; their numbers are no longer in flight.
    fn drop(&mut self) {
        if !self.evt_added.is_empty() {
            self.store.outbox.settle(&std::mem::take(&mut self.evt_added), false);
        }
    }
}
