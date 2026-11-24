use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::json::JsonValue;
use crate::model::{current_epoch_secs, parse_iso8601_utc, ProviderUsage, QuotaWindow};
use crate::providers::resolve_binary;

const AGY_TIMEOUT_SECS: u64 = 35;

pub fn fetch_antigravity_usage() -> ProviderUsage {
    let now = current_epoch_secs();
    let bin = resolve_binary("agy", "AGY_BIN");

    let mut child = match Command::new(&bin)
        .args(["-p", "/usage", "--output-format", "json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return error_usage(
                now,
                &format!("Failed to start '{} -p /usage': {}", bin.display(), e),
            );
        }
    };

    let mut stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return error_usage(now, "Failed to capture stdout from agy");
        }
    };

    let (tx, rx) = mpsc::channel::<Result<String, String>>();
    thread::spawn(move || {
        let mut buf = String::new();
        match stdout.read_to_string(&mut buf) {
            Ok(_) => {
                let _ = tx.send(Ok(buf));
            }
            Err(e) => {
                let _ = tx.send(Err(format!("Failed reading agy stdout: {}", e)));
            }
        }
    });

    let read_res = rx.recv_timeout(Duration::from_secs(AGY_TIMEOUT_SECS));
    let _ = child.kill();
    let _ = child.wait();

    match read_res {
        Ok(Ok(raw)) => parse_antigravity_response(&raw, now),
        Ok(Err(e)) => error_usage(now, &e),
        Err(_) => error_usage(
            now,
            &format!("Timed out after {}s waiting for agy /usage", AGY_TIMEOUT_SECS),
        ),
    }
}

pub fn parse_antigravity_response(raw_output: &str, now: i64) -> ProviderUsage {
    // Find the JSON object line in case any log lines preceded it
    let json_line = raw_output
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with('{') && l.ends_with('}'))
        .unwrap_or_else(|| raw_output.trim());

    let parsed = match JsonValue::parse(json_line) {
        Ok(v) => v,
        Err(e) => return error_usage(now, &format!("Invalid JSON from agy: {}", e)),
    };

    let Some(groups) = parsed
        .get_path(&["command", "data", "groups"])
        .and_then(|v| v.as_array())
    else {
        return error_usage(now, "Missing command.data.groups in agy /usage output");
    };

    let mut windows = Vec::new();

    for group in groups {
        let group_name = group
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("Models");
        let short_group = shorten_group_name(group_name);

        let Some(buckets) = group.get("buckets").and_then(|v| v.as_array()) else {
            continue;
        };

        let mut group_windows: Vec<(i32, QuotaWindow)> = Vec::new();
        for bucket in buckets {
            let Some(rem_frac) = bucket.get("remaining_fraction").and_then(|v| v.as_f64()) else {
                continue;
            };
            let window_kind = bucket
                .get("window")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let (win_suffix, sort_order) = match window_kind {
                "5h" => ("5h", 0),
                "weekly" => ("Weekly", 1),
                other if !other.is_empty() => (other, 2),
                _ => ("Quota", 2),
            };
            let label = format!("{} ({})", short_group, win_suffix);
            let has_active_usage = bucket.get("description").and_then(|v| v.as_str()).is_some()
                || rem_frac < 0.9999;
            let reset_epoch = if has_active_usage {
                bucket
                    .get("reset_time")
                    .and_then(|v| v.as_str())
                    .and_then(parse_iso8601_utc)
            } else {
                // When remaining_fraction == 1 and description is absent, the rolling window
                // has not started yet (agy returns now + window_duration on every poll).
                None
            };

            let w = QuotaWindow::from_remaining(label, rem_frac * 100.0, reset_epoch);
            group_windows.push((sort_order, w));
        }

        group_windows.sort_by_key(|(ord, _)| *ord);
        for (_, w) in group_windows {
            windows.push(w);
        }
    }

    ProviderUsage {
        provider: "Antigravity".to_string(),
        status: "ok".to_string(),
        source: "official-cli".to_string(),
        plan: None,
        extra_note: None,
        windows,
        updated_at_epoch: now,
        error_message: None,
    }
}

fn shorten_group_name(name: &str) -> &str {
    match name {
        "Gemini Models" => "Gemini",
        "Claude and GPT models" => "Claude/GPT",
        other => other,
    }
}

fn error_usage(now: i64, msg: &str) -> ProviderUsage {
    ProviderUsage {
        provider: "Antigravity".to_string(),
        status: "error".to_string(),
        source: "official-cli".to_string(),
        plan: None,
        extra_note: None,
        windows: Vec::new(),
        updated_at_epoch: now,
        error_message: Some(msg.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_antigravity_response() {
        let raw = r#"{"conversation_id":"","status":"SUCCESS","command":{"name":"usage","data":{"groups":[{"name":"Gemini Models","buckets":[{"id":"gemini-weekly","name":"Weekly Limit Remaining","window":"weekly","remaining_fraction":0.98998,"reset_time":"2026-11-28T07:30:44Z"},{"id":"gemini-5h","name":"Five Hour Limit Remaining","window":"5h","remaining_fraction":0.98993,"reset_time":"2026-11-21T12:30:44Z"}]},{"name":"Claude and GPT models","buckets":[{"id":"3p-weekly","name":"Weekly Limit Remaining","window":"weekly","remaining_fraction":1,"reset_time":"2026-11-28T08:34:48Z"},{"id":"3p-5h","name":"Five Hour Limit Remaining","window":"5h","remaining_fraction":1,"reset_time":"2026-11-21T13:34:48Z"}]},{"name":"Gemini 4 Argon","buckets":[{"id":"gemini-argon-wtu-weekly","name":"Gemini 4 Argon Weekly Limit","window":"weekly","remaining_fraction":0.93986,"reset_time":"2026-11-28T07:29:08Z"}]}]}}}"#;
        let usage = parse_antigravity_response(raw, 1795250000);
        assert_eq!(usage.status, "ok");
        assert_eq!(usage.windows.len(), 5);
        // 5h should come before Weekly within each group
        assert_eq!(usage.windows[0].label, "Gemini (5h)");
        assert!((usage.windows[0].remaining_percent - 98.993).abs() < 0.01);
        assert_eq!(usage.windows[1].label, "Gemini (Weekly)");
        assert_eq!(usage.windows[2].label, "Claude/GPT (5h)");
        assert_eq!(usage.windows[3].label, "Claude/GPT (Weekly)");
        assert_eq!(usage.windows[4].label, "Gemini 4 Argon (Weekly)");
    }
}
