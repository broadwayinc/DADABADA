//! Lexicographic key encoding. Every query is one range scan over these bytes.
//!
//! * components are joined with `0x00`; names and values never contain it
//! * `0xFF` never appears in UTF-8, so `prefix + 0xFF` is the exclusive end of "starts with prefix"
//! * typed values start with a type byte so each type sorts on its own

use serde::{Deserialize, Serialize};

pub const SEP: u8 = 0x00;
pub const END: u8 = 0xFF;

pub const T_BOOL: u8 = 0x01;
pub const T_NUM: u8 = 0x02;
pub const T_STR: u8 = 0x03;

// view bytes (first byte of a key in `ix`)
pub const V_COLL: u8 = b'C';
pub const V_USER: u8 = b'U';
pub const V_REF: u8 = b'R';
pub const V_PRIV: u8 = b'P';
pub const V_REFBY: u8 = b'X'; // X target \0 user \0 record_id : who references what
pub const V_OWNER: u8 = b'O'; // O user \0 record_id : one key per record, for listings and user deletion

// sort bytes
pub const S_REC: u8 = b'r';
pub const S_UPD: u8 = b'u';
pub const S_REFD: u8 = b'f';
pub const S_IDX: u8 = b'i';
pub const S_REV: u8 = b'v';
pub const S_LOOSE: u8 = b'l';
pub const S_LOOSE_REV: u8 = b'm';
pub const S_INI: u8 = b'k';
pub const S_INI_REV: u8 = b'n';

pub const RECORD_ID_LEN: usize = 16;

/// An index value: boolean, number or string.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Val {
    Bool(bool),
    Num(f64),
    Str(String),
}

impl Val {
    pub fn type_byte(&self) -> u8 {
        match self {
            Val::Bool(_) => T_BOOL,
            Val::Num(_) => T_NUM,
            Val::Str(_) => T_STR,
        }
    }
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Val::Bool(b) => serde_json::Value::Bool(*b),
            Val::Num(n) => num_json(*n),
            Val::Str(s) => serde_json::Value::String(s.clone()),
        }
    }
}

/// Numbers that are whole come back as JSON integers.
pub fn num_json(n: f64) -> serde_json::Value {
    if n.fract() == 0.0 && n.abs() < 9.007_199_254_740_992e15 {
        serde_json::Value::from(n as i64)
    } else {
        serde_json::Value::from(n)
    }
}

/// f64 as 8 big-endian bytes whose byte order is numeric order (negatives included).
pub fn enc_num(f: f64) -> [u8; 8] {
    let f = if f == 0.0 { 0.0 } else { f }; // -0.0 == 0.0
    let bits = f.to_bits();
    let bits = if bits >> 63 == 0 { bits ^ (1 << 63) } else { !bits };
    bits.to_be_bytes()
}

pub fn dec_num(b: &[u8]) -> f64 {
    let bits = u64::from_be_bytes(b[..8].try_into().unwrap());
    let bits = if bits >> 63 == 1 { bits ^ (1 << 63) } else { !bits };
    f64::from_bits(bits)
}

/// Type byte + payload.
pub fn typed(v: &Val) -> Vec<u8> {
    let mut out = vec![v.type_byte()];
    match v {
        Val::Bool(b) => out.push(*b as u8),
        Val::Num(n) => out.extend_from_slice(&enc_num(*n)),
        Val::Str(s) => out.extend_from_slice(s.as_bytes()),
    }
    out
}

pub fn typed_str(s: &str) -> Vec<u8> {
    let mut out = vec![T_STR];
    out.extend_from_slice(s.as_bytes());
    out
}

/// Where a view key lives: view byte, then `owner\0 collection\0 level\0 sub\0 tag\0`
/// (no owner component for the collection view). Empty sub / tag mean "none".
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Scope {
    pub view: u8,
    pub owner: String,
    pub coll: String,
    pub lvl: String,
    pub sub: String,
    pub tag: String,
}

impl Scope {
    pub fn bytes(&self) -> Vec<u8> {
        let mut k = Vec::with_capacity(64);
        k.push(self.view);
        if self.view != V_COLL {
            push_comp(&mut k, &self.owner);
        }
        push_comp(&mut k, &self.coll);
        push_comp(&mut k, &self.lvl);
        push_comp(&mut k, &self.sub);
        push_comp(&mut k, &self.tag);
        k
    }
}

pub fn push_comp(k: &mut Vec<u8>, s: &str) {
    k.extend_from_slice(s.as_bytes());
    k.push(SEP);
}

/// Sort part for an index-like sort: `kind name\0 typed\0`.
pub fn sort_idx(kind: u8, name: &str, typed_val: &[u8]) -> Vec<u8> {
    let mut k = Vec::with_capacity(2 + name.len() + typed_val.len() + 1);
    k.push(kind);
    k.extend_from_slice(name.as_bytes());
    k.push(SEP);
    k.extend_from_slice(typed_val);
    k.push(SEP);
    k
}

pub fn sort_u64(kind: u8, n: u64) -> Vec<u8> {
    let mut k = vec![kind];
    k.extend_from_slice(&n.to_be_bytes());
    k
}

pub fn record_id_of(key: &[u8]) -> &str {
    let s = &key[key.len().saturating_sub(RECORD_ID_LEN)..];
    std::str::from_utf8(s).unwrap_or("")
}

/// Key position where the sort byte starts (after the scope), for view keys C/U/R/P.
pub fn sort_pos(key: &[u8]) -> Option<usize> {
    let comps = match key.first()? {
        &V_COLL => 4,
        &V_USER | &V_REF | &V_PRIV => 5,
        _ => return None,
    };
    let mut seen = 0;
    for (i, b) in key.iter().enumerate().skip(1) {
        if *b == SEP {
            seen += 1;
            if seen == comps {
                return Some(i + 1);
            }
        }
    }
    None
}

pub fn refby_prefix(target: &str) -> Vec<u8> {
    let mut k = vec![V_REFBY];
    push_comp(&mut k, target);
    k
}

pub fn refby_key(target: &str, user: &str, record_id: &str) -> Vec<u8> {
    let mut k = refby_prefix(target);
    push_comp(&mut k, user);
    k.extend_from_slice(record_id.as_bytes());
    k
}

pub fn owner_prefix(user: &str) -> Vec<u8> {
    let mut k = vec![V_OWNER];
    push_comp(&mut k, user);
    k
}

pub fn owner_key(user: &str, record_id: &str) -> Vec<u8> {
    let mut k = owner_prefix(user);
    k.extend_from_slice(record_id.as_bytes());
    k
}

/// Smallest key greater than every key starting with `p`.
pub fn prefix_end(p: &[u8]) -> Vec<u8> {
    let mut e = p.to_vec();
    e.push(END);
    e
}

pub fn cat(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(a.len() + b.len());
    v.extend_from_slice(a);
    v.extend_from_slice(b);
    v
}
