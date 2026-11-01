use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RocksSettings {
    pub block_cache_mb: u64,
    pub blob_min_kb: u64,
}

impl Default for RocksSettings {
    fn default() -> Self {
        RocksSettings { block_cache_mb: 256, blob_min_kb: 4 }
    }
}

/// Settings stored in `<data>/dadabada.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub port: u16,
    pub bind: String,
    pub tls: bool,
    pub tls_cert: Option<String>,
    pub tls_key: Option<String>,
    pub session_idle_hours: u64,
    pub notify_keep: String,
    pub private_key_ttl: String,
    pub login_free_attempts: u32,
    pub login_max_delay: String,
    /// 0 = unlimited
    pub max_indexes: usize,
    /// 0 = unlimited
    pub max_tags: usize,
    pub log: String,
    /// Web admin sessions end after this many idle minutes.
    pub admin_session_idle_minutes: u64,
    pub rocksdb: RocksSettings,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            port: 7070,
            bind: "0.0.0.0".into(),
            tls: false,
            tls_cert: None,
            tls_key: None,
            session_idle_hours: 24,
            notify_keep: "1d".into(),
            private_key_ttl: "1h".into(),
            login_free_attempts: 5,
            login_max_delay: "15m".into(),
            max_indexes: 0,
            max_tags: 0,
            log: "stdout".into(),
            admin_session_idle_minutes: 60,
            rocksdb: RocksSettings::default(),
        }
    }
}

impl Config {
    pub fn path(data: &Path) -> PathBuf {
        data.join("dadabada.toml")
    }

    pub fn load(data: &Path) -> Result<Config, String> {
        let p = Self::path(data);
        let text = std::fs::read_to_string(&p)
            .map_err(|e| format!("Could not read {}: {e}. Run `dadabada init --data {}` first.", p.display(), data.display()))?;
        toml::from_str(&text).map_err(|e| format!("Settings file {} is not valid: {e}", p.display()))
    }

    pub fn save(&self, data: &Path) -> Result<(), String> {
        let p = Self::path(data);
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&p, text).map_err(|e| format!("Could not write {}: {e}", p.display()))
    }

    pub fn notify_keep_ms(&self) -> u64 {
        parse_duration_ms(&self.notify_keep).unwrap_or(86_400_000)
    }
    pub fn private_key_ttl_ms(&self) -> u64 {
        parse_duration_ms(&self.private_key_ttl).unwrap_or(3_600_000)
    }
    pub fn login_max_delay_ms(&self) -> u64 {
        parse_duration_ms(&self.login_max_delay).unwrap_or(900_000)
    }
}

/// Parses `30s`, `30m`, `12h`, `7d` (or a bare number of seconds) into milliseconds.
pub fn parse_duration_ms(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (num, unit) = match s.char_indices().find(|(_, c)| !c.is_ascii_digit()) {
        Some((i, _)) => (&s[..i], &s[i..]),
        None => (s, "s"),
    };
    let n: u64 = num.parse().ok()?;
    let mult = match unit.trim() {
        "ms" => 1,
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        "w" => 604_800_000,
        _ => return None,
    };
    n.checked_mul(mult)
}
