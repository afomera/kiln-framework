//! Human-readable text for times, counts and sizes: what the `time_ago`, `number`,
//! `compact_number`, `filesize` and `duration` template filters print, callable from Rust
//! for views that precompute strings. English only: `,` groups thousands, `.` is the
//! decimal point, and dates use English month names, in UTC.
//!
//! Every unit is rounded down (2h59m is "2 hours ago", 2,899 is "2.8k"), so nothing reads
//! as more than it is; `filesize` is the exception and rounds to the nearest tenth.

use crate::Timestamp;

/// How long ago `at` was, from now (the test clock in `#[kiln::test]`): see [`time_ago_from`].
pub fn time_ago(at: Timestamp) -> String {
    time_ago_from(at, Timestamp::now())
}

/// How long before `now` `at` was, rounded down to its largest unit:
///
/// | elapsed           | text                              |
/// |-------------------|-----------------------------------|
/// | under 45 seconds  | `just now`                        |
/// | under an hour     | `1 minute ago`, `59 minutes ago`  |
/// | under a day       | `1 hour ago`, `23 hours ago`      |
/// | 24 to 48 hours    | `yesterday`                       |
/// | under a week      | `2 days ago` .. `6 days ago`      |
/// | under 30 days     | `1 week ago` .. `4 weeks ago`     |
/// | 30 days or more   | `Mar 11` (`Mar 11, 2025` in another year) |
///
/// Times after `now` read the same way forward: `in 3 minutes`, `in 2 hours`, `tomorrow`,
/// `in 3 days`, `in 2 weeks`, then the date.
pub fn time_ago_from(at: Timestamp, now: Timestamp) -> String {
    let secs = now.as_jiff().as_second() - at.as_jiff().as_second();
    let future = secs < 0;
    let secs = secs.unsigned_abs();
    let span = |n: u64, unit: &str| {
        let unit = if n == 1 { unit.to_owned() } else { format!("{unit}s") };
        if future { format!("in {n} {unit}") } else { format!("{n} {unit} ago") }
    };
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    match secs {
        0..45 => "just now".into(),
        45..HOUR => span((secs / MINUTE).max(1), "minute"),
        HOUR..DAY => span(secs / HOUR, "hour"),
        DAY..172_800 => (if future { "tomorrow" } else { "yesterday" }).into(),
        172_800..604_800 => span(secs / DAY, "day"),
        604_800..2_592_000 => span(secs / (7 * DAY), "week"),
        _ => date(at, now),
    }
}

/// `Mar 11`, or `Mar 11, 2025` when `at` isn't in `now`'s year (UTC).
fn date(at: Timestamp, now: Timestamp) -> String {
    let utc = |ts: Timestamp| ts.as_jiff().to_zoned(jiff::tz::TimeZone::UTC);
    let (at, now) = (utc(at), utc(now));
    if at.year() == now.year() { at.strftime("%b %-d").to_string() } else { at.strftime("%b %-d, %Y").to_string() }
}

/// The full time `time_ago_tag` shows on hover: `Oct 18, 2026 14:03 UTC`.
pub fn full_time(at: Timestamp) -> String {
    at.as_jiff().strftime("%b %-d, %Y %H:%M UTC").to_string()
}

/// `2841` → `2,841`; `-1234567` → `-1,234,567`.
pub fn number(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let grouped = group(&digits);
    if n < 0 { format!("-{grouped}") } else { grouped }
}

/// `1234.5` → `1,234.5`; with `decimals`, rounded to that many places (`number_f64(2.0/3.0,
/// Some(2))` → `0.67`).
pub fn number_f64(n: f64, decimals: Option<usize>) -> String {
    if !n.is_finite() {
        return n.to_string();
    }
    let text = match decimals {
        Some(places) => format!("{:.*}", places, n.abs()),
        None => n.abs().to_string(),
    };
    let (whole, fraction) = text.split_once('.').map_or((text.as_str(), None), |(w, f)| (w, Some(f)));
    let sign = if n < 0.0 && text.chars().any(|c| c.is_ascii_digit() && c != '0') { "-" } else { "" };
    match fraction {
        Some(fraction) => format!("{sign}{}.{fraction}", group(whole)),
        None => format!("{sign}{}", group(whole)),
    }
}

fn group(digits: &str) -> String {
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Counts for badges and stats, rounded down: `999` → `999`, `2841` → `2.8k`,
/// `12_399` → `12.3k`, `123_456` → `123k`, `1_250_000` → `1.2M`, `3_000_000_000` → `3B`.
pub fn compact_number(n: i64) -> String {
    let sign = if n < 0 { "-" } else { "" };
    let n = n.unsigned_abs();
    let units = [(1_000_000_000_000, "T"), (1_000_000_000, "B"), (1_000_000, "M"), (1_000, "k")];
    let Some(&(size, unit)) = units.iter().find(|(size, _)| n >= *size) else {
        return format!("{sign}{n}");
    };
    let whole = n / size;
    // One decimal below 100 of a unit (`12.3k`), none above (`123k`).
    let tenths = if whole < 100 { (n % size) * 10 / size } else { 0 };
    if tenths == 0 { format!("{sign}{whole}{unit}") } else { format!("{sign}{whole}.{tenths}{unit}") }
}

/// Byte counts in binary units (1 KB = 1,024 bytes), to the nearest tenth: `870` → `870 B`,
/// `2765` → `2.7 KB`, `1_048_576` → `1 MB`, `5_905_580_032` → `5.5 GB`.
pub fn filesize(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut size = bytes as f64;
    let mut units = ["KB", "MB", "GB", "TB", "PB"].iter().peekable();
    loop {
        size /= 1024.0;
        let unit = units.next().expect("checked below");
        // `1023.96 KB` rounds to `1024.0`, so it reads as the next unit.
        if (size * 10.0).round() / 10.0 < 1024.0 || units.peek().is_none() {
            let text = format!("{size:.1}");
            return format!("{} {unit}", text.strip_suffix(".0").unwrap_or(&text));
        }
    }
}

/// Elapsed seconds, two units at most, rounded down: `38` → `38s`, `271` → `4m 31s`,
/// `302` → `5m 02s`, `3840` → `1h 04m`, `183_600` → `2d 3h`. Negative reads as `0s`.
pub fn duration(secs: i64) -> String {
    let secs = secs.max(0);
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m {:02}s", secs / 60, secs % 60),
        3600..86_400 => format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60),
        _ => format!("{}d {}h", secs / 86_400, (secs % 86_400) / 3600),
    }
}

#[cfg(test)]
mod tests {
    use jiff::SignedDuration;

    use super::*;

    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn time_ago_rounds_down_and_reads_forward() {
        let now = at("2026-10-18T12:00:00Z");
        let ago = |secs: i64| time_ago_from(now - SignedDuration::from_secs(secs), now);
        assert_eq!(ago(0), "just now");
        assert_eq!(ago(44), "just now");
        assert_eq!(ago(45), "1 minute ago");
        assert_eq!(ago(3 * 60 + 59), "3 minutes ago");
        assert_eq!(ago(3600), "1 hour ago");
        assert_eq!(ago(2 * 3600), "2 hours ago");
        assert_eq!(ago(2 * 3600 + 30 * 60), "2 hours ago");
        assert_eq!(ago(2 * 3600 + 59 * 60), "2 hours ago");
        assert_eq!(ago(24 * 3600), "yesterday");
        assert_eq!(ago(47 * 3600), "yesterday");
        assert_eq!(ago(4 * 86_400), "4 days ago");
        assert_eq!(ago(7 * 86_400), "1 week ago");
        assert_eq!(ago(23 * 86_400), "3 weeks ago");
        assert_eq!(ago(29 * 86_400), "4 weeks ago");
        assert_eq!(time_ago_from(at("2026-03-11T09:00:00Z"), now), "Mar 11");
        assert_eq!(time_ago_from(at("2025-03-11T09:00:00Z"), now), "Mar 11, 2025");

        let ahead = |secs: i64| time_ago_from(now + SignedDuration::from_secs(secs), now);
        assert_eq!(ahead(10), "just now");
        assert_eq!(ahead(5 * 60), "in 5 minutes");
        assert_eq!(ahead(3600), "in 1 hour");
        assert_eq!(ahead(30 * 3600), "tomorrow");
        assert_eq!(ahead(3 * 86_400), "in 3 days");
        assert_eq!(ahead(14 * 86_400), "in 2 weeks");
        assert_eq!(time_ago_from(at("2027-01-02T00:00:00Z"), now), "Jan 2, 2027");
    }

    #[test]
    fn numbers() {
        assert_eq!(number(0), "0");
        assert_eq!(number(999), "999");
        assert_eq!(number(2841), "2,841");
        assert_eq!(number(-1_234_567), "-1,234,567");
        assert_eq!(number(i64::MIN), "-9,223,372,036,854,775,808");
        assert_eq!(number_f64(1234.5, None), "1,234.5");
        assert_eq!(number_f64(2.0 / 3.0, Some(2)), "0.67");
        assert_eq!(number_f64(-1234567.891, Some(1)), "-1,234,567.9");
        assert_eq!(number_f64(-0.001, Some(1)), "0.0");

        assert_eq!(compact_number(999), "999");
        assert_eq!(compact_number(1000), "1k");
        assert_eq!(compact_number(2841), "2.8k");
        assert_eq!(compact_number(2899), "2.8k");
        assert_eq!(compact_number(12_399), "12.3k");
        assert_eq!(compact_number(123_456), "123k");
        assert_eq!(compact_number(999_999), "999k");
        assert_eq!(compact_number(1_250_000), "1.2M");
        assert_eq!(compact_number(3_000_000_000), "3B");
        assert_eq!(compact_number(-2841), "-2.8k");

        assert_eq!(filesize(0), "0 B");
        assert_eq!(filesize(870), "870 B");
        assert_eq!(filesize(1024), "1 KB");
        assert_eq!(filesize(2765), "2.7 KB");
        assert_eq!(filesize(1_048_575), "1 MB");
        assert_eq!(filesize(1_048_576), "1 MB");
        assert_eq!(filesize(5_905_580_032), "5.5 GB");
        assert!(filesize(u64::MAX).ends_with(" PB"));

        assert_eq!(duration(-5), "0s");
        assert_eq!(duration(38), "38s");
        assert_eq!(duration(271), "4m 31s");
        assert_eq!(duration(302), "5m 02s");
        assert_eq!(duration(3840), "1h 04m");
        assert_eq!(duration(183_600), "2d 3h");
    }
}
