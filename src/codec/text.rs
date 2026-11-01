use crate::error::ApiError;
use unicode_normalization::UnicodeNormalization;

pub fn nfc(s: &str) -> String {
    s.nfc().collect()
}

/// Reverses by Unicode character, so the result is still valid UTF-8.
pub fn rev_chars(s: &str) -> String {
    s.chars().rev().collect()
}

/// Reverses each `.` segment in place: `band.house.title` -> `dnab.esuoh.eltit`.
pub fn rev_name(name: &str) -> String {
    name.split('.').map(rev_chars).collect::<Vec<_>>().join(".")
}

fn has_control(s: &str) -> bool {
    s.chars().any(|c| c.is_control() || c == '\u{10FFFF}')
}

/// Collection names, tags and index names: 1..=256 characters, no control characters, stored as NFC.
pub fn clean_name(what: &str, s: &str) -> Result<String, ApiError> {
    let s = nfc(s);
    let n = s.chars().count();
    if n == 0 {
        return Err(ApiError::invalid(format!("The {what} is empty.")));
    }
    if n > 256 {
        return Err(ApiError::invalid(format!("The {what} is {n} characters long; the most is 256.")));
    }
    if has_control(&s) {
        return Err(ApiError::invalid(format!("The {what} \"{}\" contains a control character.", s.escape_debug())));
    }
    Ok(s)
}

/// String index values: 0..=256 characters, no control characters, stored as NFC.
pub fn clean_value(s: &str) -> Result<String, ApiError> {
    let s = nfc(s);
    let n = s.chars().count();
    if n > 256 {
        return Err(ApiError::invalid(format!("An index string value is {n} characters long; the most is 256.")));
    }
    if has_control(&s) {
        return Err(ApiError::invalid("An index string value contains a control character."));
    }
    Ok(s)
}

/// unique_id: 1..=1000 bytes, no control characters, stored as NFC.
pub fn clean_unique_id(s: &str) -> Result<String, ApiError> {
    let s = nfc(s);
    if s.is_empty() {
        return Err(ApiError::invalid("unique_id is empty."));
    }
    if s.len() > 1000 {
        return Err(ApiError::invalid(format!("unique_id is {} bytes long; the most is 1000.", s.len())));
    }
    if has_control(&s) {
        return Err(ApiError::invalid("unique_id contains a control character."));
    }
    Ok(s)
}
