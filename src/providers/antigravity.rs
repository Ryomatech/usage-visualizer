use std::env;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::json::JsonValue;
use crate::model::{current_epoch_secs, parse_iso8601_utc, ProviderUsage, QuotaWindow};
use crate::providers::{cache_dir, resolve_binary};

const AGY_TIMEOUT_SECS: u64 = 35;
const AGY_GRACEFUL_EXIT_SECS: u64 = 5;

enum AgyEvent {
    StdoutJson(String),
    StdoutDone(String),
    AuthRequired,
    Error(String),
}

/// Creates a directory containing no-op `open` and `xdg-open` scripts so that
/// `agy` child processes can NEVER launch a browser window even if OAuth expires.
fn ensure_no_browser_bin_dir() -> Option<PathBuf> {
    let dir = cache_dir().join("no_browser_bin");
    fs::create_dir_all(&dir).ok()?;
    for cmd in &["open", "xdg-open"] {
        let script_path = dir.join(cmd);
        if !script_path.exists() {
            fs::write(&script_path, "#!/bin/sh\nexit 0\n").ok()?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&script_path, fs::Permissions::from_mode(0o700));
            }
        }
    }
    Some(dir)
}

fn build_no_browser_path() -> String {
    let orig_path = env::var("PATH").unwrap_or_default();
    if let Some(shim_dir) = ensure_no_browser_bin_dir() {
        if orig_path.is_empty() {
            shim_dir.display().to_string()
        } else {
            format!("{}:{}", shim_dir.display(), orig_path)
        }
    } else {
        orig_path
    }
}

pub fn fetch_antigravity_usage() -> ProviderUsage {
    let now = current_epoch_secs();
    let bin = resolve_binary("agy", "AGY_BIN");

    let mut cmd = Command::new(&bin);
    cmd.args(["-p", "/usage", "--output-format", "json"])
        .env("PATH", build_no_browser_path())
        .env("BROWSER", "/usr/bin/true")
        .env("ANTIGRAVITY_REMOTE_OOB_AUTH", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return error_usage(
                now,
                &format!("Failed to start '{} -p /usage': {}", bin.display(), e),
            );
        }
    };

    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return error_usage(now, "Failed to capture stdout from agy");
        }
    };
    let stderr = match child.stderr.take() {
        Some(s) => s,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return error_usage(now, "Failed to capture stderr from agy");
        }
    };

    let (tx, rx) = mpsc::channel::<AgyEvent>();
    let tx_out = tx.clone();

    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut all_out = String::new();
        for line_res in reader.lines() {
            match line_res {
                Ok(line) => {
                    let trimmed = line.trim();
                    if trimmed.contains("Authentication required")
                        || trimmed.contains("Please visit the URL to log in")
                    {
                        let _ = tx_out.send(AgyEvent::AuthRequired);
                        return;
                    }
                    if trimmed.starts_with('{') && trimmed.ends_with('}') && trimmed.contains("\"usage\"") {
                        let _ = tx_out.send(AgyEvent::StdoutJson(trimmed.to_string()));
                        return;
                    }
                    all_out.push_str(&line);
                    all_out.push('\n');
                }
                Err(e) => {
                    let _ = tx_out.send(AgyEvent::Error(format!("Failed reading agy stdout: {}", e)));
                    return;
                }
            }
        }
        let _ = tx_out.send(AgyEvent::StdoutDone(all_out));
    });

    thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line_res in reader.lines() {
            let Ok(line) = line_res else { break };
            if line.contains("Authentication required")
                || line.contains("Please visit the URL to log in")
                || line.contains("triggering interactive OAuth")
                || line.contains("Your browser should open automatically")
            {
                let _ = tx.send(AgyEvent::AuthRequired);
                return;
            }
        }
    });

    let event = rx.recv_timeout(Duration::from_secs(AGY_TIMEOUT_SECS));

    match event {
        Ok(AgyEvent::StdoutJson(json_line)) => {
            // Wait gracefully for agy to finish shutting down and flushing any refreshed OAuth token
            wait_gracefully_or_kill(&mut child, AGY_GRACEFUL_EXIT_SECS);
            parse_antigravity_response(&json_line, now)
        }
        Ok(AgyEvent::StdoutDone(raw)) => {
            wait_gracefully_or_kill(&mut child, AGY_GRACEFUL_EXIT_SECS);
            if raw.contains("Authentication required")
                || raw.contains("Please visit the URL to log in")
            {
                auth_required_usage(now)
            } else {
                parse_antigravity_response(&raw, now)
            }
        }
        Ok(AgyEvent::AuthRequired) => {
            // Immediately terminate agy so it doesn't sit waiting 60s for interactive login
            let _ = child.kill();
            let _ = child.wait();
            auth_required_usage(now)
        }
        Ok(AgyEvent::Error(e)) => {
            let _ = child.kill();
            let _ = child.wait();
            error_usage(now, &e)
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            error_usage(
                now,
                &format!("Timed out after {}s waiting for agy /usage", AGY_TIMEOUT_SECS),
            )
        }
    }
}

fn wait_gracefully_or_kill(child: &mut std::process::Child, max_secs: u64) {
    let deadline = Instant::now() + Duration::from_secs(max_secs);
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
}

pub fn parse_antigravity_response(raw_output: &str, now: i64) -> ProviderUsage {
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

fn auth_required_usage(now: i64) -> ProviderUsage {
    ProviderUsage {
        provider: "Antigravity".to_string(),
        status: "waiting".to_string(),
        source: "official-cli".to_string(),
        plan: None,
        extra_note: None,
        windows: Vec::new(),
        updated_at_epoch: now,
        error_message: Some(
            "Antigravity CLI session expired. Run 'agy' once in terminal to sign in".to_string(),
        ),
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
        let raw = r#"{"conversation_id":"","status":"SUCCESS","command":{"name":"usage","data":{"groups":[{"name":"Gemini Models","buckets":[{"id":"gemini-weekly","name":"Weekly Limit Remaining","description":"Used some","window":"weekly","remaining_fraction":0.98998,"reset_time":"2026-11-28T07:30:44Z"},{"id":"gemini-5h","name":"Five Hour Limit Remaining","description":"Used some","window":"5h","remaining_fraction":0.98993,"reset_time":"2026-11-21T12:30:44Z"}]},{"name":"Claude and GPT models","buckets":[{"id":"3p-weekly","name":"Weekly Limit Remaining","window":"weekly","remaining_fraction":1,"reset_time":"2026-11-28T08:34:48Z"},{"id":"3p-5h","name":"Five Hour Limit Remaining","window":"5h","remaining_fraction":1,"reset_time":"2026-11-21T13:34:48Z"}]},{"name":"Gemini 4 Argon","buckets":[{"id":"gemini-argon-wtu-weekly","name":"Gemini 4 Argon Weekly Limit","description":"Used some","window":"weekly","remaining_fraction":0.93986,"reset_time":"2026-11-28T07:29:08Z"}]}]}}}"#;
        let usage = parse_antigravity_response(raw, 1795250000);
        assert_eq!(usage.status, "ok");
        assert_eq!(usage.windows.len(), 5);
        assert_eq!(usage.windows[0].label, "Gemini (5h)");
        assert!((usage.windows[0].remaining_percent - 98.993).abs() < 0.01);
        assert!(usage.windows[0].resets_at_epoch.is_some());
        // Unused 100% bucket without description should have resets_at_epoch == None
        assert_eq!(usage.windows[2].label, "Claude/GPT (5h)");
        assert_eq!(usage.windows[2].resets_at_epoch, None);
    }
}
