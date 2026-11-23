use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::json::JsonValue;
use crate::model::{current_epoch_secs, parse_iso8601_utc, ProviderUsage, QuotaWindow};
use crate::providers::{cache_dir, home_dir, resolve_binary, write_private_file};

const CLAUDE_RPC_TIMEOUT_SECS: u64 = 10;
const CLAUDE_GRACEFUL_EXIT_SECS: u64 = 5;

#[derive(Debug)]
pub enum ClaudeControlResult {
    Ok(ProviderUsage),
    NotLoggedIn,
    Error(String),
}

pub fn claude_cache_path() -> PathBuf {
    if let Some(custom) = env::var_os("CLAUDE_USAGE_CACHE_FILE") {
        return PathBuf::from(custom);
    }
    cache_dir().join("claude_statusline.json")
}

pub fn claude_settings_path() -> PathBuf {
    if let Some(cfg_dir) = env::var_os("CLAUDE_CONFIG_DIR") {
        return PathBuf::from(cfg_dir).join("settings.json");
    }
    home_dir().join(".claude").join("settings.json")
}

/// Fetches Claude Code usage via the official CLI control_request `get_usage` (0 tokens consumed),
/// ensuring graceful process exit (closing stdin and waiting for exit) so OAuth token refreshes
/// are flushed cleanly to macOS Keychain.
pub fn fetch_claude_usage() -> ProviderUsage {
    let now = current_epoch_secs();
    let cache_path = claude_cache_path();

    match fetch_claude_via_control_request(now) {
        ClaudeControlResult::Ok(live) => {
            let _ = write_private_file(&cache_path, &live.to_json().to_string_pretty());
            return live;
        }
        ClaudeControlResult::NotLoggedIn => {
            // Only use cache if it came from a recent statusLine hook whose reset window hasn't expired
            if let Some(valid_cache) = read_unexpired_cache(&cache_path, now) {
                if valid_cache.source == "statusline-hook" {
                    return valid_cache;
                }
            }
            return ProviderUsage {
                provider: "Claude Code".to_string(),
                status: "waiting".to_string(),
                source: "official-cli".to_string(),
                plan: None,
                extra_note: None,
                windows: Vec::new(),
                updated_at_epoch: now,
                error_message: Some(
                    "Claude CLI session expired. Run 'claude auth login' in terminal, then restart"
                        .to_string(),
                ),
            };
        }
        ClaudeControlResult::Error(err_msg) => {
            if let Some(mut valid_cache) = read_unexpired_cache(&cache_path, now) {
                valid_cache.status = "cached".to_string();
                return valid_cache;
            }
            return ProviderUsage {
                provider: "Claude Code".to_string(),
                status: "error".to_string(),
                source: "official-cli".to_string(),
                plan: None,
                extra_note: None,
                windows: Vec::new(),
                updated_at_epoch: now,
                error_message: Some(err_msg),
            };
        }
    }
}

fn fetch_claude_via_control_request(now: i64) -> ClaudeControlResult {
    let bin = resolve_binary("claude", "CLAUDE_BIN");

    let mut child = match Command::new(&bin)
        .args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return ClaudeControlResult::Error(format!(
                "Failed to spawn '{}': {}",
                bin.display(),
                e
            ))
        }
    };

    let mut stdin = match child.stdin.take() {
        Some(s) => s,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return ClaudeControlResult::Error("Failed to open stdin for claude".to_string());
        }
    };
    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return ClaudeControlResult::Error("Failed to open stdout for claude".to_string());
        }
    };

    let (tx, rx) = mpsc::channel::<Option<String>>();

    thread::spawn(move || {
        let req = r#"{"type":"control_request","request_id":"usage-1","request":{"subtype":"get_usage","skip_behaviors":true}}"#;
        if writeln!(stdin, "{}", req).and_then(|_| stdin.flush()).is_err() {
            let _ = tx.send(None);
            return;
        }

        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        for _ in 0..15 {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = line.trim();
                    if let Ok(val) = JsonValue::parse(trimmed) {
                        if val.get("type").and_then(|v| v.as_str()) == Some("control_response") {
                            // Drop stdin immediately so `claude` flushes any refreshed OAuth tokens
                            // to macOS Keychain and exits cleanly with code 0!
                            drop(stdin);
                            let _ = tx.send(Some(trimmed.to_string()));
                            return;
                        }
                    }
                }
                Err(_) => break,
            }
        }
        drop(stdin);
        let _ = tx.send(None);
    });

    let raw_resp = rx
        .recv_timeout(Duration::from_secs(CLAUDE_RPC_TIMEOUT_SECS))
        .ok()
        .flatten();

    // Wait gracefully for `claude` to finish flushing Keychain/config before exiting.
    // Never send SIGKILL immediately after control_response, or an in-flight OAuth token rotation
    // can be interrupted before the new refreshToken is saved to macOS Keychain.
    let deadline = Instant::now() + Duration::from_secs(CLAUDE_GRACEFUL_EXIT_SECS);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }

    match raw_resp {
        Some(raw) => parse_claude_control_response(&raw, now),
        None => ClaudeControlResult::Error("Timed out waiting for claude get_usage".to_string()),
    }
}

pub fn parse_claude_control_response(raw_json: &str, now: i64) -> ClaudeControlResult {
    let Ok(parsed) = JsonValue::parse(raw_json.trim()) else {
        return ClaudeControlResult::Error("Invalid JSON in claude control_response".to_string());
    };
    let Some(payload) = parsed.get_path(&["response", "response"]) else {
        return ClaudeControlResult::Error(
            "Missing response.response in claude control_response".to_string(),
        );
    };

    let available = payload
        .get("rate_limits_available")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !available {
        return ClaudeControlResult::NotLoggedIn;
    }

    let Some(rate_limits) = payload.get("rate_limits") else {
        return ClaudeControlResult::NotLoggedIn;
    };
    if matches!(rate_limits, JsonValue::Null) {
        return ClaudeControlResult::NotLoggedIn;
    }

    let plan = payload
        .get("subscription_type")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let mut windows = Vec::new();
    let known_buckets = [
        ("five_hour", "5h window"),
        ("seven_day", "Weekly (7d)"),
        ("seven_day_opus", "Weekly Opus"),
        ("seven_day_sonnet", "Weekly Sonnet"),
    ];

    for (key, label) in &known_buckets {
        if let Some(bucket) = rate_limits.get(key) {
            if let Some(w) = parse_claude_control_window(bucket, label) {
                windows.push(w);
            }
        }
    }

    if let Some(scoped) = rate_limits.get("model_scoped").and_then(|v| v.as_array()) {
        for item in scoped {
            let name = item
                .get("display_name")
                .and_then(|v| v.as_str())
                .unwrap_or("Model");
            if let Some(w) = parse_claude_control_window(item, &format!("{} (Weekly)", name)) {
                windows.push(w);
            }
        }
    }

    if windows.is_empty() {
        return ClaudeControlResult::NotLoggedIn;
    }

    ClaudeControlResult::Ok(ProviderUsage {
        provider: "Claude Code".to_string(),
        status: "ok".to_string(),
        source: "official-cli".to_string(),
        plan,
        extra_note: None,
        windows,
        updated_at_epoch: now,
        error_message: None,
    })
}

fn parse_claude_control_window(bucket: &JsonValue, label: &str) -> Option<QuotaWindow> {
    let used = bucket
        .get("utilization")
        .and_then(|v| v.as_f64())
        .or_else(|| bucket.get("used_percentage").and_then(|v| v.as_f64()))?;
    let reset_epoch = bucket.get("resets_at").and_then(|v| {
        v.as_str()
            .and_then(parse_iso8601_utc)
            .or_else(|| v.as_i64())
    });
    Some(QuotaWindow::from_used(label, used, reset_epoch))
}

/// Reads the cached Claude usage only if its reset windows have not all expired in the past.
fn read_unexpired_cache(cache_path: &Path, now: i64) -> Option<ProviderUsage> {
    let raw = fs::read_to_string(cache_path).ok()?;
    let parsed = JsonValue::parse(&raw).ok()?;
    let usage = ProviderUsage::from_json(&parsed)?;
    if usage.windows.is_empty() {
        return None;
    }
    // If all windows have a resets_at_epoch in the past (now >= reset), the cache is from a
    // previous window cycle and does NOT know the new cycle's reset time or consumption.
    let has_active_window = usage
        .windows
        .iter()
        .any(|w| w.resets_at_epoch.map(|r| r > now).unwrap_or(false));
    if !has_active_window {
        return None;
    }
    Some(usage)
}

pub fn read_claude_cache(cache_path: &Path, now: i64) -> ProviderUsage {
    if let Some(valid) = read_unexpired_cache(cache_path, now) {
        return valid;
    }
    ProviderUsage {
        provider: "Claude Code".to_string(),
        status: "waiting".to_string(),
        source: "official-cli".to_string(),
        plan: None,
        extra_note: None,
        windows: Vec::new(),
        updated_at_epoch: now,
        error_message: Some(
            "Claude CLI session expired. Run 'claude auth login' in terminal, then restart"
                .to_string(),
        ),
    }
}

/// Processes the JSON piped by Claude Code's `statusLine` feature on stdin,
/// updates the local rate-limit cache file if `rate_limits` is present,
/// and returns a compact status bar line for Claude Code's TUI.
pub fn process_statusline_payload(
    raw_json: &str,
    cache_path: &Path,
    now: i64,
) -> Result<String, String> {
    let parsed = JsonValue::parse(raw_json.trim())?;

    let model_name = parsed
        .get_path(&["model", "display_name"])
        .and_then(|v| v.as_str())
        .or_else(|| parsed.get_path(&["model", "id"]).and_then(|v| v.as_str()))
        .unwrap_or("Claude");

    let ctx_used = parsed
        .get_path(&["context_window", "used_percentage"])
        .and_then(|v| v.as_f64());

    let five_used = parsed
        .get_path(&["rate_limits", "five_hour", "used_percentage"])
        .and_then(|v| v.as_f64());
    let five_reset = parsed
        .get_path(&["rate_limits", "five_hour", "resets_at"])
        .and_then(|v| v.as_i64());

    let seven_used = parsed
        .get_path(&["rate_limits", "seven_day", "used_percentage"])
        .and_then(|v| v.as_f64());
    let seven_reset = parsed
        .get_path(&["rate_limits", "seven_day", "resets_at"])
        .and_then(|v| v.as_i64());

    if five_used.is_some() || seven_used.is_some() {
        let mut windows = Vec::new();
        if let Some(used) = five_used {
            windows.push(QuotaWindow::from_used("5h window", used, five_reset));
        }
        if let Some(used) = seven_used {
            windows.push(QuotaWindow::from_used("Weekly (7d)", used, seven_reset));
        }
        let snapshot = ProviderUsage {
            provider: "Claude Code".to_string(),
            status: "ok".to_string(),
            source: "statusline-hook".to_string(),
            plan: None,
            extra_note: Some(format!("Model: {}", model_name)),
            windows,
            updated_at_epoch: now,
            error_message: None,
        };
        write_private_file(cache_path, &snapshot.to_json().to_string_pretty())?;
    }

    let mut parts = vec![model_name.to_string()];
    if let Some(ctx) = ctx_used {
        parts.push(format!("Ctx: {:.0}%", ctx));
    }
    if let Some(u5) = five_used {
        let rem5 = (100.0 - u5).clamp(0.0, 100.0);
        parts.push(format!("5h rem: {:.0}%", rem5));
    }
    if let Some(u7) = seven_used {
        let rem7 = (100.0 - u7).clamp(0.0, 100.0);
        parts.push(format!("7d rem: {:.0}%", rem7));
    }
    Ok(parts.join(" | "))
}

pub fn configure_claude_settings(
    settings_path: &Path,
    hook_command: &str,
) -> Result<String, String> {
    let mut root_map = if settings_path.exists() {
        let raw = fs::read_to_string(settings_path)
            .map_err(|e| format!("Failed to read {}: {}", settings_path.display(), e))?;
        match JsonValue::parse(&raw)? {
            JsonValue::Object(m) => m,
            _ => return Err("~/.claude/settings.json root is not a JSON object".to_string()),
        }
    } else {
        BTreeMap::new()
    };

    let mut sl_map = BTreeMap::new();
    sl_map.insert("type".to_string(), JsonValue::String("command".to_string()));
    sl_map.insert(
        "command".to_string(),
        JsonValue::String(hook_command.to_string()),
    );
    root_map.insert("statusLine".to_string(), JsonValue::Object(sl_map));

    let updated = JsonValue::Object(root_map).to_string_pretty() + "\n";
    if let Some(parent) = settings_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create {}: {}", parent.display(), e))?;
    }
    fs::write(settings_path, &updated)
        .map_err(|e| format!("Failed to write {}: {}", settings_path.display(), e))?;
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env::temp_dir;

    #[test]
    fn test_parse_claude_control_response() {
        let raw = r#"{
            "type": "control_response",
            "response": {
                "subtype": "success",
                "request_id": "usage-1",
                "response": {
                    "subscription_type": "max",
                    "rate_limits_available": true,
                    "rate_limits": {
                        "five_hour": {
                            "utilization": 32.0,
                            "resets_at": "2026-11-28T12:30:00Z"
                        },
                        "seven_day": {
                            "utilization": 14.0,
                            "resets_at": "2026-11-28T07:00:00Z"
                        }
                    }
                }
            }
        }"#;
        let res = parse_claude_control_response(raw, 1795250000);
        let ClaudeControlResult::Ok(usage) = res else {
            panic!("Expected Ok usage");
        };
        assert_eq!(usage.status, "ok");
        assert_eq!(usage.plan.as_deref(), Some("max"));
        assert_eq!(usage.windows.len(), 2);
        assert_eq!(usage.windows[0].label, "5h window");
        assert_eq!(usage.windows[0].remaining_percent, 68.0);
        assert_eq!(usage.windows[1].label, "Weekly (7d)");
        assert_eq!(usage.windows[1].remaining_percent, 86.0);
    }

    #[test]
    fn test_expired_cache_is_not_treated_as_ok() {
        let tmp_cache = temp_dir().join("usage_vis_test_claude_expired_cache.json");
        let _ = fs::remove_file(&tmp_cache);

        let payload = r#"{
            "model": {"display_name": "Claude Opus"},
            "context_window": {"used_percentage": 18.2},
            "rate_limits": {
                "five_hour": {"used_percentage": 28.0, "resets_at": 1800000000},
                "seven_day": {"used_percentage": 12.0, "resets_at": 1800500000}
            }
        }"#;

        process_statusline_payload(payload, &tmp_cache, 1790000000).expect("hook ok");

        // Before reset: cache is valid
        let usage = read_claude_cache(&tmp_cache, 1790000100);
        assert_eq!(usage.status, "ok");
        assert_eq!(usage.windows.len(), 2);

        // After both windows reset and no new data arrived: must NOT pretend 100% with missing Resets In
        let expired = read_claude_cache(&tmp_cache, 1800600000);
        assert_eq!(expired.status, "waiting");
        assert!(expired.windows.is_empty());

        let _ = fs::remove_file(&tmp_cache);
    }
}
