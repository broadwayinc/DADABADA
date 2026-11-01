use rand::Rng;
use sha2::{Digest, Sha256};

/// Base62 in ASCII order, so a fixed-width base62 string sorts like the number.
pub const B62: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn base62_fixed(mut n: u64, width: usize) -> String {
    let mut out = vec![b'0'; width];
    for i in (0..width).rev() {
        out[i] = B62[(n % 62) as usize];
        n /= 62;
    }
    String::from_utf8(out).unwrap()
}

pub fn base62_decode(s: &str) -> Option<u64> {
    let mut n: u64 = 0;
    for b in s.bytes() {
        let v = B62.iter().position(|&c| c == b)? as u64;
        n = n.checked_mul(62)?.checked_add(v)?;
    }
    Some(n)
}

pub fn random_b62(len: usize) -> String {
    let mut rng = rand::thread_rng();
    (0..len).map(|_| B62[rng.gen_range(0..62)] as char).collect()
}

/// 4 base62 characters derived from a user id (ownership pre-check, as in skapi).
pub fn user_hash4(user_id: &str) -> String {
    let h = Sha256::digest(user_id.as_bytes());
    let n = u64::from_be_bytes(h[..8].try_into().unwrap());
    base62_fixed(n % 62u64.pow(4), 4)
}

/// 16 characters: 7 base62 time (ms) + 5 random + 4 from the uploader.
pub fn new_record_id(user_id: &str, ts: u64) -> String {
    format!("{}{}{}", base62_fixed(ts, 7), random_b62(5), user_hash4(user_id))
}

pub fn record_uploaded(record_id: &str) -> u64 {
    record_id.get(..7).and_then(base62_decode).unwrap_or(0)
}

pub fn is_record_id(s: &str) -> bool {
    s.len() == 16 && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

pub fn new_user_id() -> String {
    random_b62(8)
}

pub fn new_key_id() -> String {
    random_b62(6)
}
