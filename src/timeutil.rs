use std::time::{SystemTime, UNIX_EPOCH};

fn local_now() -> Option<(libc::tm, u32)> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    let seconds = now.as_secs() as libc::time_t;
    let micros = now.subsec_micros();
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let result = unsafe { libc::localtime_r(&seconds, &mut tm) };
    (!result.is_null()).then_some((tm, micros))
}

/// Local time formatted like Python's `datetime.now().astimezone()
/// .isoformat(timespec="seconds")`, e.g. `2026-10-26T12:34:56+08:00`.
pub fn local_timestamp() -> String {
    let Some((tm, _)) = local_now() else {
        return String::new();
    };
    let offset = tm.tm_gmtoff;
    let sign = if offset < 0 { '-' } else { '+' };
    let absolute = offset.abs();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{}{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        sign,
        absolute / 3600,
        absolute % 3600 / 60
    )
}

/// Local time formatted like naive `datetime.now().isoformat()`, including
/// microseconds and no offset, e.g. `2026-10-26T12:34:56.123456`.
pub fn local_iso_timestamp() -> String {
    let Some((tm, micros)) = local_now() else {
        return String::new();
    };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        micros
    )
}

/// `strftime("%Y%m%d_%H%M%S")`.
pub fn local_compact_timestamp() -> String {
    let Some((tm, _)) = local_now() else {
        return String::new();
    };
    format!(
        "{:04}{:02}{:02}_{:02}{:02}{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// `strftime("%Y%m%d-%H%M%S")`.
pub fn local_compact_stamp() -> String {
    local_compact_timestamp().replacen('_', "-", 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_timestamp_has_microseconds_and_no_offset() {
        let value = local_iso_timestamp();
        assert_eq!(value.len(), "2026-10-26T12:34:56.123456".len());
        assert_eq!(value.get(10..11), Some("T"));
        assert_eq!(value.get(19..20), Some("."));
        assert!(!value.contains('+'));
        assert!(!value.ends_with('Z'));
    }

    #[test]
    fn compact_stamps_replace_only_the_separator() {
        let timestamp = local_compact_timestamp();
        let stamp = local_compact_stamp();
        assert_eq!(timestamp.len(), 15);
        assert_eq!(stamp.len(), 15);
        assert_eq!(timestamp.replacen('_', "-", 1), stamp);
    }
}
