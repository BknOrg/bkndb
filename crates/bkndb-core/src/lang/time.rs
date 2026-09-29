//! Minimal ISO 8601 / RFC 3339 timestamp and UUID text parsing for query
//! literals (`TIMESTAMP '2026-01-31T12:00:00Z'`, `UUID '...'`), without
//! pulling in a date-time dependency.

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

fn num(s: &str) -> Option<i64> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok()).flatten()
}

/// Parses `YYYY-MM-DD`, optionally followed by `T` (or a space) and
/// `HH:MM[:SS[.fraction]]`, optionally followed by `Z` or `±HH:MM`. A value
/// without an offset is taken as UTC. Returns microseconds since the epoch.
pub(crate) fn parse_timestamp(text: &str) -> Option<i64> {
    let s = text.trim();
    let (date, rest) = if s.len() > 10 { s.split_at(10) } else { (s, "") };
    let mut parts = date.split('-');
    let (y, m, d) = (num(parts.next()?)?, num(parts.next()?)?, num(parts.next()?)?);
    if parts.next().is_some() || date.len() != 10 || !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
        return None;
    }
    let mut micros = days_from_civil(y, m, d) * 86_400_000_000;
    if rest.is_empty() {
        return Some(micros);
    }
    let rest = rest.strip_prefix('T').or_else(|| rest.strip_prefix('t')).or_else(|| rest.strip_prefix(' '))?;
    // Split off the offset.
    let (clock, offset_micros) = if let Some(c) = rest.strip_suffix('Z').or_else(|| rest.strip_suffix('z')) {
        (c, 0)
    } else if let Some(idx) = rest.rfind(['+', '-']) {
        let (c, off) = rest.split_at(idx);
        let sign = if off.starts_with('-') { -1 } else { 1 };
        let (oh, om) = off[1..].split_once(':')?;
        let (oh, om) = (num(oh)?, num(om)?);
        if oh > 23 || om > 59 {
            return None;
        }
        (c, sign * (oh * 3600 + om * 60) * 1_000_000)
    } else {
        (rest, 0)
    };
    let mut fields = clock.split(':');
    let h = num(fields.next()?)?;
    let mi = num(fields.next()?)?;
    let (sec, frac) = match fields.next() {
        Some(sf) => match sf.split_once('.') {
            Some((s, f)) => (num(s)?, f),
            None => (num(sf)?, ""),
        },
        None => (0, ""),
    };
    if fields.next().is_some() || h > 23 || mi > 59 || sec > 59 {
        return None;
    }
    let frac_micros = if frac.is_empty() {
        0
    } else {
        num(frac)?;
        let digits: String = frac.chars().chain(std::iter::repeat('0')).take(6).collect();
        digits.parse::<i64>().ok()?
    };
    micros += ((h * 60 + mi) * 60 + sec) * 1_000_000 + frac_micros;
    micros.checked_sub(offset_micros)
}

/// Parses a UUID in its hex form, with or without hyphens (and optional
/// surrounding braces).
pub(crate) fn parse_uuid(text: &str) -> Option<[u8; 16]> {
    let hex: String = text.trim().trim_start_matches('{').trim_end_matches('}').chars().filter(|&c| c != '-').collect();
    if hex.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_timestamps() {
        assert_eq!(parse_timestamp("1970-01-01"), Some(0));
        assert_eq!(parse_timestamp("1970-01-02T00:00:00Z"), Some(86_400_000_000));
        assert_eq!(parse_timestamp("2000-03-01 00:00"), Some(951_868_800_000_000));
        assert_eq!(parse_timestamp("1969-12-31T23:59:59.5Z"), Some(-500_000));
        assert_eq!(parse_timestamp("2024-02-29T12:00:00+07:00"), parse_timestamp("2024-02-29T05:00:00Z"));
        assert_eq!(parse_timestamp("2026-09-29T10:20:30.123456-01:30"), parse_timestamp("2026-09-29T11:50:30.123456Z"));
        for bad in ["2023-02-29", "2026-13-01", "2026-1-01", "2026-01-01T25:00", "nope", "2026-01-01X", "2026-01-01T10:00:00.x"] {
            assert_eq!(parse_timestamp(bad), None, "{bad}");
        }
    }

    #[test]
    fn parses_uuids() {
        let u = parse_uuid("00112233-4455-6677-8899-aabbccddeeff").unwrap();
        assert_eq!(u[0], 0x00);
        assert_eq!(u[15], 0xff);
        assert_eq!(parse_uuid("{00112233445566778899AABBCCDDEEFF}"), Some(u));
        assert_eq!(parse_uuid("0011"), None);
    }
}
