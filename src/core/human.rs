//! Small, dependency-free formatting helpers used across the UI.

/// Binary size, the unit family yt-dlp and aria2 both report in.
pub fn bytes(n: u64) -> String {
    const K: f64 = 1024.0;
    let f = n as f64;
    if n < 1024 {
        format!("{n} B")
    } else if f < K * K {
        format!("{:.1} KiB", f / K)
    } else if f < K * K * K {
        format!("{:.1} MiB", f / (K * K))
    } else if f < K * K * K * K {
        format!("{:.2} GiB", f / (K * K * K))
    } else {
        format!("{:.2} TiB", f / (K * K * K * K))
    }
}

pub fn speed(bytes_per_sec: u64) -> String {
    if bytes_per_sec == 0 {
        "--".into()
    } else {
        format!("{}/s", bytes(bytes_per_sec))
    }
}

/// `1:05` under an hour, `1:02:05` above it.
pub fn eta(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, (secs % 3600) / 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

pub fn percent(done: u64, total: Option<u64>) -> Option<f32> {
    let total = total.filter(|t| *t > 0)?;
    Some((done as f32 / total as f32).clamp(0.0, 1.0))
}

/// Fraction the progress bar should paint, keeping a visible sliver while
/// work is in flight and the total is unknown.
pub fn bar_fraction(done: u64, total: Option<u64>) -> f32 {
    match percent(done, total) {
        Some(p) => p.clamp(0.004, 1.0),
        None if done > 0 => 0.12,
        None => 0.02,
    }
}

/// How many days until a Netscape `expires` stamp, negative when already past.
pub fn days_until(expires: i64, now: i64) -> i64 {
    if expires <= 0 {
        return i64::MAX;
    }
    (expires - now) / 86_400
}

pub fn expiry_label(expires: i64, now: i64) -> String {
    if expires <= 0 {
        return "session".into();
    }
    let d = days_until(expires, now);
    if d < 0 {
        "expired".into()
    } else if d == 0 {
        "today".into()
    } else if d == 1 {
        "1 day".into()
    } else if d < 45 {
        format!("{d} days")
    } else if d < 730 {
        format!("{} mo", d / 30)
    } else {
        format!("{} y", d / 365)
    }
}

/// Trim a URL for a one-line subtitle without hiding the host.
pub fn short_url(url: &str, max: usize) -> String {
    let cleaned = url.split('?').next().unwrap_or(url);
    if cleaned.chars().count() <= max {
        return cleaned.to_string();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = cleaned.chars().take(keep).collect();
    out.push('…');
    out
}

pub fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn short_path(p: &std::path::Path, max: usize) -> String {
    let s = p.to_string_lossy().to_string();
    if s.chars().count() <= max {
        return s;
    }
    let skip = s.chars().count() - max.saturating_sub(1);
    let mut out = String::from("…");
    out.extend(s.chars().skip(skip));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_use_binary_units() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1024), "1.0 KiB");
        assert_eq!(bytes(1024 * 1024 * 3 / 2), "1.5 MiB");
    }

    #[test]
    fn eta_formats_both_ranges() {
        assert_eq!(eta(5), "0:05");
        assert_eq!(eta(65), "1:05");
        assert_eq!(eta(3725), "1:02:05");
    }

    #[test]
    fn percent_ignores_unknown_totals() {
        assert_eq!(percent(10, None), None);
        assert_eq!(percent(10, Some(0)), None);
        assert_eq!(percent(5, Some(10)), Some(0.5));
    }

    #[test]
    fn short_url_keeps_host_and_drops_query() {
        assert_eq!(short_url("https://a.test/x?b=1", 40), "https://a.test/x");
        assert!(short_url("https://a.test/aaaaaaaaaaaaaaaaaaaa", 12).ends_with('…'));
    }
}
