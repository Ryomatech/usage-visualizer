use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::json::JsonValue;

#[derive(Debug, Clone, PartialEq)]
pub struct QuotaWindow {
    pub label: String,
    pub used_percent: f64,
    pub remaining_percent: f64,
    pub resets_at_epoch: Option<i64>,
}

impl QuotaWindow {
    pub fn from_used(label: impl Into<String>, used_percent: f64, resets_at_epoch: Option<i64>) -> Self {
        let clamped_used = used_percent.clamp(0.0, 100.0);
        let remaining = (100.0 - clamped_used).clamp(0.0, 100.0);
        Self {
            label: label.into(),
            used_percent: clamped_used,
            remaining_percent: remaining,
            resets_at_epoch,
        }
    }

    pub fn from_remaining(
        label: impl Into<String>,
        remaining_percent: f64,
        resets_at_epoch: Option<i64>,
    ) -> Self {
        let clamped_rem = remaining_percent.clamp(0.0, 100.0);
        let used = (100.0 - clamped_rem).clamp(0.0, 100.0);
        Self {
            label: label.into(),
            used_percent: used,
            remaining_percent: clamped_rem,
            resets_at_epoch,
        }
    }

    /// If the reset timestamp has already passed, usage in that window has reset to 0% (100% remaining).
    pub fn normalized_for_now(&self, now_epoch: i64) -> Self {
        if let Some(reset) = self.resets_at_epoch {
            if now_epoch >= reset {
                return Self {
                    label: self.label.clone(),
                    used_percent: 0.0,
                    remaining_percent: 100.0,
                    resets_at_epoch: None,
                };
            }
        }
        self.clone()
    }

    pub fn to_json(&self) -> JsonValue {
        let mut map = BTreeMap::new();
        map.insert("label".to_string(), JsonValue::String(self.label.clone()));
        map.insert(
            "usedPercent".to_string(),
            JsonValue::Number((self.used_percent * 10.0).round() / 10.0),
        );
        map.insert(
            "remainingPercent".to_string(),
            JsonValue::Number((self.remaining_percent * 10.0).round() / 10.0),
        );
        map.insert(
            "resetsAtEpoch".to_string(),
            match self.resets_at_epoch {
                Some(ts) => JsonValue::Number(ts as f64),
                None => JsonValue::Null,
            },
        );
        map.insert(
            "resetsAtIsoUtc".to_string(),
            match self.resets_at_epoch {
                Some(ts) => JsonValue::String(epoch_to_iso_utc(ts)),
                None => JsonValue::Null,
            },
        );
        JsonValue::Object(map)
    }

    pub fn from_json(val: &JsonValue) -> Option<Self> {
        let label = val.get("label")?.as_str()?.to_string();
        let used_percent = val.get("usedPercent")?.as_f64()?;
        let remaining_percent = val.get("remainingPercent")?.as_f64()?;
        let resets_at_epoch = val.get("resetsAtEpoch").and_then(|v| v.as_i64());
        Some(Self {
            label,
            used_percent,
            remaining_percent,
            resets_at_epoch,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderUsage {
    pub provider: String,
    pub status: String,
    pub source: String,
    pub plan: Option<String>,
    pub extra_note: Option<String>,
    pub windows: Vec<QuotaWindow>,
    pub updated_at_epoch: i64,
    pub error_message: Option<String>,
}

impl ProviderUsage {
    pub fn to_json(&self) -> JsonValue {
        let mut map = BTreeMap::new();
        map.insert("provider".to_string(), JsonValue::String(self.provider.clone()));
        map.insert("status".to_string(), JsonValue::String(self.status.clone()));
        map.insert("source".to_string(), JsonValue::String(self.source.clone()));
        map.insert(
            "plan".to_string(),
            match &self.plan {
                Some(p) => JsonValue::String(p.clone()),
                None => JsonValue::Null,
            },
        );
        map.insert(
            "extraNote".to_string(),
            match &self.extra_note {
                Some(n) => JsonValue::String(n.clone()),
                None => JsonValue::Null,
            },
        );
        map.insert(
            "windows".to_string(),
            JsonValue::Array(self.windows.iter().map(|w| w.to_json()).collect()),
        );
        map.insert(
            "updatedAtEpoch".to_string(),
            JsonValue::Number(self.updated_at_epoch as f64),
        );
        map.insert(
            "updatedAtIsoUtc".to_string(),
            JsonValue::String(epoch_to_iso_utc(self.updated_at_epoch)),
        );
        map.insert(
            "errorMessage".to_string(),
            match &self.error_message {
                Some(e) => JsonValue::String(e.clone()),
                None => JsonValue::Null,
            },
        );
        JsonValue::Object(map)
    }

    pub fn from_json(val: &JsonValue) -> Option<Self> {
        let provider = val.get("provider")?.as_str()?.to_string();
        let status = val.get("status")?.as_str()?.to_string();
        let source = val.get("source")?.as_str()?.to_string();
        let plan = val.get("plan").and_then(|v| v.as_str()).map(|s| s.to_string());
        let extra_note = val
            .get("extraNote")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let windows_arr = val.get("windows")?.as_array()?;
        let mut windows = Vec::with_capacity(windows_arr.len());
        for w in windows_arr {
            windows.push(QuotaWindow::from_json(w)?);
        }
        let updated_at_epoch = val.get("updatedAtEpoch")?.as_i64()?;
        let error_message = val
            .get("errorMessage")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        Some(Self {
            provider,
            status,
            source,
            plan,
            extra_note,
            windows,
            updated_at_epoch,
            error_message,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DashboardState {
    pub updated_at_epoch: i64,
    pub providers: Vec<ProviderUsage>,
}

impl DashboardState {
    pub fn to_json(&self) -> JsonValue {
        let mut map = BTreeMap::new();
        map.insert(
            "updatedAtEpoch".to_string(),
            JsonValue::Number(self.updated_at_epoch as f64),
        );
        map.insert(
            "updatedAtIsoUtc".to_string(),
            JsonValue::String(epoch_to_iso_utc(self.updated_at_epoch)),
        );
        map.insert(
            "providers".to_string(),
            JsonValue::Array(self.providers.iter().map(|p| p.to_json()).collect()),
        );
        JsonValue::Object(map)
    }

    pub fn from_json(val: &JsonValue) -> Option<Self> {
        let updated_at_epoch = val.get("updatedAtEpoch")?.as_i64()?;
        let arr = val.get("providers")?.as_array()?;
        let mut providers = Vec::with_capacity(arr.len());
        for p in arr {
            providers.push(ProviderUsage::from_json(p)?);
        }
        Some(Self {
            updated_at_epoch,
            providers,
        })
    }
}

pub fn current_epoch_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Parses an ISO-8601 UTC timestamp like "2026-11-28T07:30:44Z" or "2026-11-28T07:30:44.123Z"
/// into Unix epoch seconds.
pub fn parse_iso8601_utc(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.len() < 19 {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    if s.as_bytes().get(4).copied()? != b'-' {
        return None;
    }
    let month: i64 = s.get(5..7)?.parse().ok()?;
    if s.as_bytes().get(7).copied()? != b'-' {
        return None;
    }
    let day: i64 = s.get(8..10)?.parse().ok()?;
    let sep = s.as_bytes().get(10).copied()?;
    if sep != b'T' && sep != b't' && sep != b' ' {
        return None;
    }
    let hour: i64 = s.get(11..13)?.parse().ok()?;
    if s.as_bytes().get(13).copied()? != b':' {
        return None;
    }
    let minute: i64 = s.get(14..16)?.parse().ok()?;
    if s.as_bytes().get(16).copied()? != b':' {
        return None;
    }
    let second: i64 = s.get(17..19)?.parse().ok()?;

    let days = days_from_civil(year, month, day);
    let mut epoch = days * 86400 + hour * 3600 + minute * 60 + second;

    // Optional timezone offset (+HH:MM or -HH:MM) after index 19
    let rest = &s[19..];
    if let Some(pos) = rest.find(|c| c == '+' || c == '-') {
        let tz = &rest[pos..];
        if tz.len() >= 6 {
            let sign = if tz.starts_with('+') { 1i64 } else { -1i64 };
            let tz_h: i64 = tz.get(1..3)?.parse().ok()?;
            let tz_m: i64 = tz.get(4..6)?.parse().ok()?;
            epoch -= sign * (tz_h * 3600 + tz_m * 60);
        }
    }
    Some(epoch)
}

/// Howard Hinnant's civil-to-days algorithm (days since 1970-01-01)
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Inverse of days_from_civil: days since 1970-01-01 -> (year, month, day)
pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    (y + if m <= 2 { 1 } else { 0 }, m, d)
}

pub fn epoch_to_iso_utc(epoch: i64) -> String {
    let days = epoch.div_euclid(86400);
    let secs_of_day = epoch.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    let h = secs_of_day / 3600;
    let min = (secs_of_day % 3600) / 60;
    let s = secs_of_day % 60;
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, m, d, h, min, s)
}

/// Formats an epoch timestamp in local timezone offset (defaults to +09:00 JST or TZ_OFFSET_MINS env)
pub fn format_local_datetime(epoch: i64, offset_secs: i64, include_date: bool) -> String {
    let local = epoch + offset_secs;
    let days = local.div_euclid(86400);
    let secs_of_day = local.rem_euclid(86400);
    let (_, m, d) = civil_from_days(days);
    let h = secs_of_day / 3600;
    let min = (secs_of_day % 3600) / 60;
    let s = secs_of_day % 60;
    if include_date {
        format!("{:02}/{:02} {:02}:{:02}", m, d, h, min)
    } else {
        format!("{:02}:{:02}:{:02}", h, min, s)
    }
}

pub fn format_reset_countdown(resets_at_epoch: Option<i64>, now_epoch: i64, offset_secs: i64) -> String {
    let Some(target) = resets_at_epoch else {
        return "—".to_string();
    };
    let diff = target - now_epoch;
    if diff <= 0 {
        return "Reset".to_string();
    }
    let days = diff / 86400;
    let hours = (diff % 86400) / 3600;
    let mins = (diff % 3600) / 60;

    let rel = if days > 0 {
        format!("in {}d {:02}h", days, hours)
    } else if hours > 0 {
        format!("in {}h {:02}m", hours, mins)
    } else {
        format!("in {}m", mins.max(1))
    };

    let local_str = if days > 0 {
        format_local_datetime(target, offset_secs, true)
    } else {
        let local = target + offset_secs;
        let secs_of_day = local.rem_euclid(86400);
        format!("{:02}:{:02}", secs_of_day / 3600, (secs_of_day % 3600) / 60)
    };
    format!("{} ({})", rel, local_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_iso8601_parse_and_format() {
        let iso = "2026-11-28T07:30:44Z";
        let ts = parse_iso8601_utc(iso).expect("parse iso");
        assert_eq!(epoch_to_iso_utc(ts), iso);
    }

    #[test]
    fn test_window_reset_normalization() {
        let w = QuotaWindow::from_used("5h window", 75.0, Some(1000));
        let before = w.normalized_for_now(900);
        assert_eq!(before.remaining_percent, 25.0);

        let after = w.normalized_for_now(1001);
        assert_eq!(after.remaining_percent, 100.0);
        assert_eq!(after.used_percent, 0.0);
    }
}
