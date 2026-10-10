//! HIST-004: the contract grammar guards every use of calendar arithmetic.

use crate::{Error, ErrorCode, Input, Value, identity::staged};
use chrono::{Datelike, NaiveDate, TimeDelta, Timelike};

/// Normalize exactly the shared timestamp input grammar to six-digit uppercase UTC.
/// Unknown time is field absence, not a null/sentinel accepted by this primitive.
pub fn normalize_timestamp(input: Input<'_>) -> Result<String, Error> {
    let values = staged(&[("timestamp", input)])?;
    timestamp_rule(&values[0])
}

pub(crate) fn timestamp_rule(value: &Value) -> Result<String, Error> {
    let invalid = || Error::new(ErrorCode::TimestampInvalid);
    let text = value.as_str().ok_or_else(invalid)?;
    if !text.is_ascii() || text.len() < 20 {
        return Err(invalid());
    }
    let bytes = text.as_bytes();
    let number = |from: usize, to: usize| -> Result<u32, Error> {
        let span = bytes.get(from..to).ok_or_else(invalid)?;
        if !span.iter().all(u8::is_ascii_digit) {
            return Err(invalid());
        }
        Ok(span.iter().fold(0, |v, c| v * 10 + u32::from(c - b'0')))
    };
    if bytes[4] != b'-'
        || bytes[7] != b'-'
        || !matches!(bytes[10], b'T' | b't')
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return Err(invalid());
    }
    let year = number(0, 4)? as i32;
    let month = number(5, 7)?;
    let day = number(8, 10)?;
    let hour = number(11, 13)?;
    let minute = number(14, 16)?;
    let second = number(17, 19)?;
    if !(1..=9999).contains(&year) || second > 59 {
        return Err(invalid());
    }
    let mut at = 19;
    let mut micro = 0;
    if bytes[at] == b'.' {
        at += 1;
        let start = at;
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
        if !(1..=6).contains(&(at - start)) {
            return Err(invalid());
        }
        micro = number(start, at)? * 10_u32.pow(6 - (at - start) as u32);
    }
    let offset = match bytes.get(at) {
        Some(b'Z' | b'z') if at + 1 == bytes.len() => 0,
        Some(b'+' | b'-') if at + 6 == bytes.len() && bytes[at + 3] == b':' => {
            let h = number(at + 1, at + 3)?;
            let m = number(at + 4, at + 6)?;
            if h > 23 || m > 59 {
                return Err(invalid());
            }
            i64::from(h * 60 + m) * if bytes[at] == b'+' { 1 } else { -1 }
        }
        _ => return Err(invalid()),
    };
    let local = NaiveDate::from_ymd_opt(year, month, day)
        .and_then(|date| date.and_hms_micro_opt(hour, minute, second, micro))
        .ok_or_else(invalid)?;
    let utc = local
        .checked_sub_signed(TimeDelta::minutes(offset))
        .ok_or_else(invalid)?;
    if !(1..=9999).contains(&utc.year()) {
        return Err(invalid());
    }
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}Z",
        utc.year(),
        utc.month(),
        utc.day(),
        utc.hour(),
        utc.minute(),
        utc.second(),
        utc.nanosecond() / 1000
    ))
}
