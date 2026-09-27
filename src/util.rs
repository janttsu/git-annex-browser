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

/// `YYYY-MM-DD HH:MM UTC`. git-annex log timestamps are UTC.
pub fn fmt_unix(ts: i64) -> String {
    if ts <= 0 {
        return "never".into();
    }
    let days = ts.div_euclid(86_400);
    let secs = ts.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        secs / 3600,
        (secs % 3600) / 60
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
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
    fn fmt_unix_matches_known_dates() {
        assert_eq!(fmt_unix(0), "never");
        assert_eq!(fmt_unix(1_317_929_189), "2011-10-06 19:26 UTC");
        assert_eq!(fmt_unix(951_782_400), "2000-02-29 00:00 UTC");
        assert_eq!(fmt_unix(1_790_000_000), "2026-09-21 14:13 UTC");
    }

    #[test]
    fn short_uuid_is_char_safe() {
        assert_eq!(short_uuid("ääääääääää"), "ääääääää…");
        assert_eq!(short_uuid("abc"), "abc");
    }
}
