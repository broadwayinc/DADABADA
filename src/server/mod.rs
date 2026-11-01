pub mod admin_sock;
pub mod http;
pub mod listen;
pub mod tls;
pub mod ui;
pub mod web_admin;

/// The server log: every line goes to stdout and into a ring the web admin can read
/// (`logs` admin command). The ring keeps the last LOG_KEEP lines; each line has a
/// sequence number so a reader asks for "everything after N".
pub const LOG_KEEP: usize = 1000;
pub static LOG: std::sync::Mutex<(u64, std::collections::VecDeque<(u64, String)>)> =
    std::sync::Mutex::new((0, std::collections::VecDeque::new()));

pub fn log(line: String) {
    println!("{line}");
    let mut g = LOG.lock().unwrap();
    g.0 += 1;
    let seq = g.0;
    if g.1.len() >= LOG_KEEP {
        g.1.pop_front();
    }
    g.1.push_back((seq, line));
}

/// Log lines after `after` (0 = everything kept), oldest first.
pub fn log_since(after: u64) -> (u64, Vec<(u64, String)>) {
    let g = LOG.lock().unwrap();
    (g.0, g.1.iter().filter(|(s, _)| *s > after).cloned().collect())
}

/// HH:MM:SS in local time.
pub fn clock() -> String {
    let t = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}
