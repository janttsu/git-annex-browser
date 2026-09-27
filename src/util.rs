use chrono::{DateTime, Utc};

pub fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if n == 0 {
        return "0 B".into();
    }
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
    if ts <= 0 {
        return "never".into();
    }
    let dt = DateTime::<Utc>::from_timestamp(ts, 0).unwrap_or_default();
    dt.format("%Y-%m-%d %H:%M").to_string()
}

pub fn short_uuid(u: &str) -> String {
    if u.chars().count() > 8 {
        format!("{}…", u.chars().take(8).collect::<String>())
    } else {
        u.to_string()
    }
}

/// Keep the end of `s` so it fits in `max` characters, prefixing `...` when cut.
/// Works on characters, so multi-byte names never split mid-codepoint.
pub fn truncate_start(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(3);
    let tail: String = s.chars().skip(count - keep).collect();
    format!("...{tail}")
}

pub fn trust_color(trust: crate::annex::TrustLevel) -> ratatui::style::Color {
    use crate::annex::TrustLevel::*;
    use ratatui::style::Color;
    match trust {
        Trusted => Color::Green,
        SemiTrusted => Color::Yellow,
        UnTrusted => Color::Red,
        Dead => Color::DarkGray,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_start_is_char_safe() {
        let name = "äöäöäöäöäöäöäöäöäöäöäöäöäöäöäöäöäöäöäöäöäöäö";
        let t = truncate_start(name, 10);
        assert_eq!(t.chars().count(), 10);
        assert!(t.starts_with("..."));
        assert_eq!(truncate_start("short", 10), "short");
    }

    #[test]
    fn short_uuid_is_char_safe() {
        assert_eq!(short_uuid("ääääääääää"), "ääääääää…");
        assert_eq!(short_uuid("abc"), "abc");
    }
}
