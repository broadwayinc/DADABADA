use crate::codec::body::Meta;
use serde::{Deserialize, Serialize};

/// Background work, written to the outbox in the same batch as the change that caused it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum Event {
    /// Collection counters: records (+1/-1) and size in bytes.
    Coll { coll: String, level: String, d: i64, size: i64 },
    /// User record counter.
    UserRecords { user: String, d: i64 },
    Tag { coll: String, tag: String, d: i64 },
    /// kind: T_BOOL / T_NUM / T_STR; num: the number (or 1/0 for booleans)
    Idx { coll: String, name: String, kind: u8, num: f64, d: i64 },
    /// unique id added (+1) or removed (-1): keeps the reversed listing
    Uniq { id: String, record_id: String, d: i64 },
    /// referenced_count changed: move the record's `f` sort keys
    RefCount { record_id: String },
    /// A record was deleted; `cascade` when something referenced it
    Removed { record_id: String, meta: Meta, cascade: bool },
    /// Put a record in feeds: subscribers of `uploader`, or only `to`
    Feed { record_id: String, uploader: String, level: String, to: Option<String> },
    /// Send a notification: subscribers of `uploader` with get_notified, or only `to`
    Notify { record_id: String, uploader: String, collection: String, title: String, body: String, to: Option<String> },
    /// Subscriber / subscribed counters on user rows
    SubCount { user: String, subscribers: i64, subscribed: i64 },
    /// Remove everything a deleted user left behind, a page at a time
    UserDeleted { user_id: String },
}
