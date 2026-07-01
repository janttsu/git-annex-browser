use chrono::{DateTime, Utc};

pub fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if n == 0 { return "0 B".into(); }
    let mut size = n as f64;
    let mut i = 0;
    while size >= 1024.0 && i < UNITS.len() - 1 {
        size /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", n, UNITS[0])
    } else {
        format!("{:.1} {}", size, UNITS[i])
    }
}

pub fn fmt_unix(ts: i64) -> String {
    if ts <= 0 { return "never".into(); }
    let dt = DateTime::<Utc>::from_timestamp(ts, 0).unwrap_or_default();
    dt.format("%Y-%m-%d %H:%M").to_string()
}

pub fn short_uuid(u: &str) -> String {
    if u.len() > 8 { format!("{}…", &u[..8]) } else { u.to_string() }
}

// trust_color / now reserved for future colorized / live views
#[allow(dead_code)]
pub fn trust_color(trust: crate::annex::TrustLevel) -> ratatui::style::Color {
    use ratatui::style::Color;
    use crate::annex::TrustLevel::*;
    match trust {
        Trusted => Color::Green,
        SemiTrusted => Color::Yellow,
        UnTrusted => Color::Red,
        Dead => Color::DarkGray,
    }
}

#[allow(dead_code)]
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}