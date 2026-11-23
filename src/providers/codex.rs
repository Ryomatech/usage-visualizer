use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::json::JsonValue;
use crate::model::{current_epoch_secs, ProviderUsage, QuotaWindow};
use crate::providers::resolve_binary;

const CODEX_TIMEOUT_SECS: u64 = 8;

pub fn fetch_codex_usage() -> ProviderUsage {
    let now = current_epoch_secs();
    let bin = resolve_binary("codex", "CODEX_BIN");

    let mut child = match Command::new(&bin)
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return ProviderUsage {
                provider: "Codex".to_string(),
                status: "error".to_string(),
                source: "official-jsonrpc".to_string(),
                plan: None,
                extra_note: None,
                windows: Vec::new(),
                updated_at_epoch: now,
                error_message: Some(format!("Failed to start '{} app-server': {}", bin.display(), e)),
            };
        }
    };

    let mut stdin = match child.stdin.take() {
        Some(s) => s,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return error_usage(now, "Failed to open stdin for codex app-server");
        }
    };
    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return error_usage(now, "Failed to open stdout for codex app-server");
        }
    };

    let (tx, rx) = mpsc::channel::<Result<String, String>>();

    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let init_msg = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"clientInfo":{"name":"usage-visualizer","title":"Usage Visualizer","version":"0.1.0"}}}"#;
        if let Err(e) = writeln!(stdin, "{}", init_msg).and_then(|_| stdin.flush()) {
            let _ = tx.send(Err(format!("Failed to send initialize: {}", e)));
            return;
        }

        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    let _ = tx.send(Err("EOF before initialize response".to_string()));
                    return;
                }
                Ok(_) => {
                    if let Ok(val) = JsonValue::parse(line.trim()) {
                        if val.get("id").and_then(|v| v.as_i64()) == Some(1) {
                            break;
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(format!("Read error during initialize: {}", e)));
                    return;
                }
            }
        }

        let initialized_msg = r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#;
        let read_limits_msg =
            r#"{"jsonrpc":"2.0","id":2,"method":"account/rateLimits/read","params":{}}"#;
        if let Err(e) = writeln!(stdin, "{}", initialized_msg)
            .and_then(|_| writeln!(stdin, "{}", read_limits_msg))
            .and_then(|_| stdin.flush())
        {
            let _ = tx.send(Err(format!("Failed to send rateLimits/read: {}", e)));
            return;
        }

        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    let _ = tx.send(Err("EOF before rateLimits/read response".to_string()));
                    return;
                }
                Ok(_) => {
                    let trimmed = line.trim();
                    if let Ok(val) = JsonValue::parse(trimmed) {
                        if val.get("id").and_then(|v| v.as_i64()) == Some(2) {
                            drop(stdin);
                            let _ = tx.send(Ok(trimmed.to_string()));
                            return;
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(format!("Read error waiting for rateLimits: {}", e)));
                    return;
                }
            }
        }
    });

    let rpc_result = rx.recv_timeout(Duration::from_secs(CODEX_TIMEOUT_SECS));

    // Always terminate and reap codex app-server immediately so no resident process remains
    let _ = child.kill();
    let _ = child.wait();

    match rpc_result {
        Ok(Ok(raw_json)) => parse_codex_response(&raw_json, now),
        Ok(Err(e)) => error_usage(now, &e),
        Err(_) => error_usage(
            now,
            &format!("Timed out after {}s waiting for codex app-server", CODEX_TIMEOUT_SECS),
        ),
    }
}

pub fn parse_codex_response(raw_json: &str, now: i64) -> ProviderUsage {
    let parsed = match JsonValue::parse(raw_json) {
        Ok(v) => v,
        Err(e) => return error_usage(now, &format!("Invalid JSON from codex: {}", e)),
    };

    if let Some(err_obj) = parsed.get("error") {
        let msg = err_obj
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("JSON-RPC error from codex app-server");
        return error_usage(now, msg);
    }

    let Some(result) = parsed.get("result") else {
        return error_usage(now, "Missing 'result' in codex JSON-RPC response");
    };

    let rate_limits = result.get("rateLimits");
    let plan = rate_limits
        .and_then(|rl| rl.get("planType"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let reset_credits = result
        .get_path(&["rateLimitResetCredits", "availableCount"])
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let extra_note = if reset_credits > 0 {
        Some(format!("Reset credits: {}", reset_credits))
    } else {
        None
    };

    let mut windows = Vec::new();
    if let Some(rl) = rate_limits {
        if let Some(primary) = rl.get("primary") {
            if let Some(w) = parse_codex_window(primary, "5h window") {
                windows.push(w.normalized_for_now(now));
            }
        }
        if let Some(secondary) = rl.get("secondary") {
            if let Some(w) = parse_codex_window(secondary, "Weekly (7d)") {
                windows.push(w.normalized_for_now(now));
            }
        }
    }

    // Also include any additional distinct limit buckets in rateLimitsByLimitId
    if let Some(by_id) = result.get("rateLimitsByLimitId").and_then(|v| v.as_object()) {
        for (limit_id, entry) in by_id {
            if limit_id == "codex" && !windows.is_empty() {
                continue;
            }
            let prefix = entry
                .get("limitName")
                .and_then(|v| v.as_str())
                .unwrap_or(limit_id.as_str());
            if let Some(primary) = entry.get("primary") {
                if let Some(w) = parse_codex_window(primary, &format!("{} (5h)", prefix)) {
                    windows.push(w.normalized_for_now(now));
                }
            }
            if let Some(secondary) = entry.get("secondary") {
                if let Some(w) = parse_codex_window(secondary, &format!("{} (7d)", prefix)) {
                    windows.push(w.normalized_for_now(now));
                }
            }
        }
    }

    ProviderUsage {
        provider: "Codex".to_string(),
        status: "ok".to_string(),
        source: "official-jsonrpc".to_string(),
        plan,
        extra_note,
        windows,
        updated_at_epoch: now,
        error_message: None,
    }
}

fn parse_codex_window(win: &JsonValue, default_label: &str) -> Option<QuotaWindow> {
    let used = win.get("usedPercent")?.as_f64()?;
    let resets_at = win.get("resetsAt").and_then(|v| v.as_i64());
    let label = match win.get("windowDurationMins").and_then(|v| v.as_i64()) {
        Some(300) => "5h window".to_string(),
        Some(10080) => "Weekly (7d)".to_string(),
        Some(mins) if mins % 1440 == 0 => format!("{}d window", mins / 1440),
        Some(mins) if mins % 60 == 0 => format!("{}h window", mins / 60),
        _ => default_label.to_string(),
    };
    Some(QuotaWindow::from_used(label, used, resets_at))
}

fn error_usage(now: i64, msg: &str) -> ProviderUsage {
    ProviderUsage {
        provider: "Codex".to_string(),
        status: "error".to_string(),
        source: "official-jsonrpc".to_string(),
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
    fn test_parse_codex_response() {
        let sample = r#"{
          "id": 2,
          "result": {
            "ordinaryUsageAllowed": true,
            "rateLimits": {
              "limitId": "codex",
              "primary": {
                "usedPercent": 62,
                "windowDurationMins": 300,
                "resetsAt": 1895259669
              },
              "secondary": {
                "usedPercent": 39,
                "windowDurationMins": 10080,
                "resetsAt": 1895643836
              },
              "planType": "team"
            },
            "rateLimitResetCredits": {
              "availableCount": 3
            }
          }
        }"#;
        let usage = parse_codex_response(sample, 1790000000);
        assert_eq!(usage.status, "ok");
        assert_eq!(usage.plan.as_deref(), Some("team"));
        assert_eq!(usage.extra_note.as_deref(), Some("Reset credits: 3"));
        assert_eq!(usage.windows.len(), 2);
        assert_eq!(usage.windows[0].label, "5h window");
        assert_eq!(usage.windows[0].remaining_percent, 38.0);
        assert_eq!(usage.windows[1].label, "Weekly (7d)");
        assert_eq!(usage.windows[1].remaining_percent, 61.0);
    }
}
