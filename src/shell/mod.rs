//! Shared bits of the admin and user shells.

pub mod admin;
pub mod client;

use serde_json::{json, Value};

/// Positional words and `--flag value` / `--switch` options (flags may repeat).
pub struct Args {
    pub pos: Vec<String>,
    pub flags: Vec<(String, Option<String>)>,
}

/// Flags that never take a value.
const SWITCHES: &[&str] = &[
    "json", "desc", "asc", "yes", "readonly", "read-only", "private", "feed", "notify", "replace", "skip-existing",
    "blocked", "ignore-case", "i", "no-index",
];

impl Args {
    pub fn parse(words: &[String]) -> Args {
        let mut pos = vec![];
        let mut flags = vec![];
        let mut i = 0;
        while i < words.len() {
            let w = &words[i];
            if let Some(f) = w.strip_prefix("--") {
                if let Some((k, v)) = f.split_once('=') {
                    flags.push((k.to_string(), Some(v.to_string())));
                } else if SWITCHES.contains(&f) || i + 1 >= words.len() {
                    flags.push((f.to_string(), None));
                } else {
                    flags.push((f.to_string(), Some(words[i + 1].clone())));
                    i += 1;
                }
            } else {
                pos.push(w.clone());
            }
            i += 1;
        }
        Args { pos, flags }
    }
    pub fn get(&self, k: &str) -> Option<&str> {
        self.flags.iter().rev().find(|(n, _)| n == k).and_then(|(_, v)| v.as_deref())
    }
    pub fn all(&self, k: &str) -> Vec<&str> {
        self.flags.iter().filter(|(n, _)| n == k).filter_map(|(_, v)| v.as_deref()).collect()
    }
    pub fn has(&self, k: &str) -> bool {
        self.flags.iter().any(|(n, _)| n == k)
    }
}

pub fn split(line: &str) -> Result<Vec<String>, String> {
    shlex::split(line).ok_or_else(|| "Unclosed quote in the command.".to_string())
}

/// `1200` -> number, `true` -> boolean, `"1200"` (quoted in the shell) stays a string.
/// A level typed at the prompt: "5" is sent as "05" (the API takes exactly two digits, or "**").
pub fn level_arg(l: &str) -> Value {
    let l = l.trim();
    if l.len() == 1 && l.as_bytes()[0].is_ascii_digit() {
        Value::String(format!("0{l}"))
    } else {
        Value::String(l.to_string())
    }
}

pub fn guess_value(s: &str) -> Value {
    let t = s.trim();
    if let Some(q) = t.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
        return json!(q);
    }
    if let Ok(n) = t.parse::<i64>() {
        return json!(n);
    }
    if let Ok(n) = t.parse::<f64>() {
        if n.is_finite() {
            return json!(n);
        }
    }
    match t {
        "true" => json!(true),
        "false" => json!(false),
        _ => json!(t),
    }
}

/// `price >= 1000`, `title ~<= .pdf`, `price 100..500`, `title = "Desk Lamp"`.
pub fn index_expr(expr: &str) -> Result<Value, String> {
    let e = expr.trim();
    for op in ["~>=", "~<=", "~=", "~>", "~<", ">=", "<=", "=", ">", "<"] {
        if let Some(pos) = find_op(e, op) {
            let name = e[..pos].trim();
            let val = e[pos + op.len()..].trim();
            if name.is_empty() || val.is_empty() {
                break;
            }
            let loose = op.starts_with('~');
            let cond = op.trim_start_matches('~');
            let mut v = json!({"name": name, "value": guess_value(val), "condition": cond});
            if loose {
                v["match"] = json!("loose");
                v["value"] = json!(val.trim_matches('"'));
            }
            return Ok(v);
        }
    }
    // name lo..hi (or name ~lo..hi)
    if let Some((name, range)) = e.split_once(char::is_whitespace) {
        let range = range.trim();
        let (loose, range) = match range.strip_prefix('~') {
            Some(r) => (true, r),
            None => (false, range),
        };
        if let Some((lo, hi)) = range.split_once("..") {
            let mut v = json!({"name": name.trim(), "value": guess_value(lo), "range": guess_value(hi)});
            if loose {
                v["match"] = json!("loose");
            }
            return Ok(v);
        }
    }
    if !e.is_empty() && !e.contains(char::is_whitespace) {
        return Ok(json!({"name": e}));
    }
    Err(format!("Could not read the index condition \"{expr}\". Use: name = value, name > value, name >= text (starts with), name <= text (ends with), name lo..hi, with ~ for loose."))
}

fn find_op(e: &str, op: &str) -> Option<usize> {
    // the first operator occurrence that is not part of a longer operator
    let mut start = 0;
    while let Some(i) = e[start..].find(op) {
        let at = start + i;
        let before = e[..at].chars().last();
        let after = e[at + op.len()..].chars().next();
        let longer = matches!(before, Some('~' | '>' | '<' | '=')) || (op.len() == 1 && after == Some('='));
        if !longer || op.starts_with('~') {
            return Some(at);
        }
        start = at + op.len();
    }
    None
}

/// One record per line: id, collection/level, indexes, tags, data.
pub fn record_line(r: &Value) -> String {
    let idx: Vec<String> = r["index"]
        .as_array()
        .map(|a| a.iter().map(|i| format!("{}={}", i["name"].as_str().unwrap_or(""), i["value"])).collect())
        .unwrap_or_default();
    let tags: Vec<&str> = r["tags"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let mut data = r["data"].to_string();
    if data.chars().count() > 80 {
        data = data.chars().take(77).collect::<String>() + "...";
    }
    let mut s = format!(
        "{}  {}/{}",
        r["record_id"].as_str().unwrap_or(""),
        r["collection"]["name"].as_str().unwrap_or(""),
        r["collection"]["access_level"].as_str().unwrap_or("")
    );
    if !idx.is_empty() {
        s += &format!("  [{}]", idx.join(" "));
    }
    if !tags.is_empty() {
        s += &format!("  tags:{}", tags.join(","));
    }
    if let Some(u) = r["unique_id"].as_str() {
        s += &format!("  unique:{u}");
    }
    if let Some(rf) = r["reference"].as_str() {
        s += &format!("  ref:{rf}");
    }
    s += &format!("  {data}");
    s
}

pub fn print_list(v: &Value, json_out: bool) {
    if json_out {
        println!("{}", serde_json::to_string_pretty(v).unwrap());
        return;
    }
    let list = v["list"].as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        println!("(nothing found)");
    }
    for r in &list {
        if r.get("record_id").is_some() && r["collection"].is_object() {
            println!("{}", record_line(r));
        } else {
            println!("{}", r);
        }
    }
    if v["end_of_list"].as_bool() == Some(false) {
        println!("({} shown; type `more` for the next page)", list.len());
    }
}

pub fn read_line_plain(prompt: &str) -> String {
    use std::io::Write;
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    let _ = std::io::stdin().read_line(&mut s);
    s.trim_end_matches(['\n', '\r']).to_string()
}
