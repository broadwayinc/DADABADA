//! A stored record: `[u32 meta length][MessagePack meta][raw JSON data]`.

use super::keys::Val;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SubCfg {
    #[serde(default)]
    pub is_subscription_record: bool,
    #[serde(default)]
    pub upload_to_feed: bool,
    #[serde(default)]
    pub notify_subscribers: bool,
    #[serde(default)]
    pub feed_referencing_records: bool,
    #[serde(default)]
    pub notify_referencing_records: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Restriction {
    pub name: String,
    #[serde(default)]
    pub value: Option<Val>,
    #[serde(default)]
    pub range: Option<Val>,
    #[serde(default)]
    pub condition: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Source {
    #[serde(default)]
    pub referencing_limit: Option<u64>,
    #[serde(default)]
    pub prevent_multiple_referencing: bool,
    #[serde(default)]
    pub can_remove_referencing_records: bool,
    #[serde(default)]
    pub only_granted_can_reference: bool,
    #[serde(default)]
    pub allow_granted_to_grant_others: bool,
    #[serde(default)]
    pub referencing_index_restrictions: Vec<Restriction>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub title: String,
    pub body: String,
}

/// The agent key that last wrote a record. Records written before the key id was kept hold the name alone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentTag {
    Key { key_id: String, name: String },
    Name(String),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub user_id: String,
    pub agent: Option<AgentTag>,
    pub collection: String,
    /// "00".."99" or "**"
    pub level: String,
    pub sub: SubCfg,
    pub index: Vec<(String, Val)>,
    pub tags: Vec<String>,
    pub reference: Option<String>,
    pub unique_id: Option<String>,
    pub source: Source,
    pub readonly: bool,
    pub uploaded: u64,
    pub updated: u64,
    /// Exact count, updated with the referencing record.
    pub referenced_count: u64,
    /// The count the `f` sort keys were last written with (moved by the worker).
    pub referenced_count_indexed: u64,
    pub ip: String,
    pub notification: Option<Notification>,
}

impl Meta {
    /// The owner view uses the subscription owner component only for subscription records.
    pub fn sub_owner(&self) -> &str {
        if self.sub.is_subscription_record {
            &self.user_id
        } else {
            ""
        }
    }
    pub fn is_private(&self) -> bool {
        self.level == "**"
    }
    pub fn level_num(&self) -> Option<u8> {
        self.level.parse().ok()
    }
}

pub fn encode(meta: &Meta, data_json: &[u8]) -> Vec<u8> {
    let m = rmp_serde::to_vec_named(meta).expect("meta encodes");
    let mut out = Vec::with_capacity(4 + m.len() + data_json.len());
    out.extend_from_slice(&(m.len() as u32).to_be_bytes());
    out.extend_from_slice(&m);
    out.extend_from_slice(data_json);
    out
}

pub fn decode(bytes: &[u8]) -> Option<(Meta, &[u8])> {
    if bytes.len() < 4 {
        return None;
    }
    let n = u32::from_be_bytes(bytes[..4].try_into().ok()?) as usize;
    let meta: Meta = rmp_serde::from_slice(bytes.get(4..4 + n)?).ok()?;
    Some((meta, &bytes[4 + n..]))
}
