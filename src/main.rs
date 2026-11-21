mod json;
mod model;
mod providers;
mod render;

use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use crate::json::JsonValue;
use crate::model::{current_epoch_secs, DashboardState, ProviderUsage};
use crate::providers::antigravity::fetch_antigravity_usage;
use crate::providers::claude::{
    claude_cache_path, claude_settings_path, configure_claude_settings, fetch_claude_usage,
    process_statusline_payload, read_claude_cache,
};
use crate::providers::codex::fetch_codex_usage;
use crate::providers::{cache_dir, write_private_file};
use crate::render::{render_dashboard, RenderOptions};

const DEFAULT_INTERVAL_SECS: u64 = 300; // 5 minutes
const DEFAULT_WARN_THRESHOLD: f64 = 20.0;
const DEFAULT_TZ_OFFSET_SECS: i64 = 9 * 3600; // +09:00 JST default (overridable via TZ_OFFSET_HOURS)

#[derive(Debug, Clone)]
struct CliConfig {
    mode: RunMode,
    interval_secs: u64,
    force_refresh: bool,
    json_output: bool,
    no_color: bool,
    warn_threshold: f64,
    tz_offset_secs: i64,
}

#[derive(Debug, Clone, PartialEq)]
enum RunMode {
    Watch,
    Once,
    ClaudeHook,
    SetupClaude,
    Help,
}

fn parse_args() -> CliConfig {
    let tz_offset_secs = env::var("TZ_OFFSET_HOURS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .map(|h| h * 3600)
        .unwrap_or(DEFAULT_TZ_OFFSET_SECS);

    let mut cfg = CliConfig {
        mode: RunMode::Watch, // Persistent watch mode is the default
        interval_secs: DEFAULT_INTERVAL_SECS,
        force_refresh: false,
        json_output: false,
        no_color: env::var_os("NO_COLOR").is_some(),
        warn_threshold: DEFAULT_WARN_THRESHOLD,
        tz_offset_secs,
    };

    let args: Vec<String> = env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "watch" | "--watch" | "-w" => cfg.mode = RunMode::Watch,
            "once" | "status" | "--once" | "-1" => cfg.mode = RunMode::Once,
            "claude-hook" | "--claude-hook" => cfg.mode = RunMode::ClaudeHook,
            "setup-claude" | "--setup-claude" => cfg.mode = RunMode::SetupClaude,
            "help" | "--help" | "-h" => cfg.mode = RunMode::Help,
            "--refresh" | "-r" => cfg.force_refresh = true,
            "--json" => {
                cfg.json_output = true;
                if cfg.mode == RunMode::Watch {
                    cfg.mode = RunMode::Once;
                }
            }
            "--no-color" => cfg.no_color = true,
            "--interval" | "-i" => {
                if let Some(val) = args.get(i + 1) {
                    if let Ok(secs) = val.parse::<u64>() {
                        cfg.interval_secs = secs.max(10);
                    }
                    i += 1;
                }
            }
            "--warn" => {
                if let Some(val) = args.get(i + 1) {
                    if let Ok(th) = val.parse::<f64>() {
                        cfg.warn_threshold = th.clamp(1.0, 99.0);
                    }
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }

    cfg
}

fn state_cache_path() -> PathBuf {
    cache_dir().join("state.json")
}

fn load_cached_state() -> Option<DashboardState> {
    let raw = fs::read_to_string(state_cache_path()).ok()?;
    let val = JsonValue::parse(&raw).ok()?;
    DashboardState::from_json(&val)
}

fn save_cached_state(state: &DashboardState) {
    let _ = write_private_file(&state_cache_path(), &state.to_json().to_string_pretty());
}

fn collect_all_providers(prev: Option<&DashboardState>) -> DashboardState {
    let now = current_epoch_secs();

    // Run all 3 providers in parallel so total latency is bounded by the slowest one
    let h_claude = thread::spawn(fetch_claude_usage);
    let h_codex = thread::spawn(fetch_codex_usage);
    let h_agy = thread::spawn(fetch_antigravity_usage);

    let claude = h_claude.join().unwrap_or_else(|_| ProviderUsage {
        provider: "Claude Code".to_string(),
        status: "error".to_string(),
        source: "statusline-hook".to_string(),
        plan: None,
        extra_note: None,
        windows: Vec::new(),
        updated_at_epoch: now,
        error_message: Some("Claude worker thread panicked".to_string()),
    });
    let codex = h_codex.join().unwrap_or_else(|_| ProviderUsage {
        provider: "Codex".to_string(),
        status: "error".to_string(),
        source: "official-jsonrpc".to_string(),
        plan: None,
        extra_note: None,
        windows: Vec::new(),
        updated_at_epoch: now,
        error_message: Some("Codex worker thread panicked".to_string()),
    });
    let agy = h_agy.join().unwrap_or_else(|_| ProviderUsage {
        provider: "Antigravity".to_string(),
        status: "error".to_string(),
        source: "official-cli".to_string(),
        plan: None,
        extra_note: None,
        windows: Vec::new(),
        updated_at_epoch: now,
        error_message: Some("Antigravity worker thread panicked".to_string()),
    });

    let providers = vec![
        fallback_to_previous_if_error(claude, prev, now),
        fallback_to_previous_if_error(codex, prev, now),
        fallback_to_previous_if_error(agy, prev, now),
    ];

    let state = DashboardState {
        updated_at_epoch: now,
        providers,
    };
    save_cached_state(&state);
    state
}

fn fallback_to_previous_if_error(
    current: ProviderUsage,
    prev: Option<&DashboardState>,
    now: i64,
) -> ProviderUsage {
    if current.status != "error" || !current.windows.is_empty() {
        return current;
    }
    if let Some(prev_state) = prev {
        if let Some(old) = prev_state
            .providers
            .iter()
            .find(|p| p.provider == current.provider && !p.windows.is_empty())
        {
            let mut cached = old.clone();
            cached.status = "cached".to_string();
            cached.windows = cached
                .windows
                .into_iter()
                .map(|w| w.normalized_for_now(now))
                .collect();
            return cached;
        }
    }
    current
}

fn get_or_refresh_state(ttl_secs: u64, force_refresh: bool) -> DashboardState {
    let now = current_epoch_secs();
    let cached = load_cached_state();
    if !force_refresh {
        if let Some(ref state) = cached {
            if now - state.updated_at_epoch < ttl_secs as i64 {
                let mut updated = state.clone();
                let cpath = claude_cache_path();
                if cpath.exists() {
                    let fresh_claude = read_claude_cache(&cpath, now);
                    if !fresh_claude.windows.is_empty() {
                        if let Some(slot) = updated
                            .providers
                            .iter_mut()
                            .find(|p| p.provider == "Claude Code")
                        {
                            *slot = fresh_claude;
                        }
                    }
                }
                return updated;
            }
        }
    }
    collect_all_providers(cached.as_ref())
}

fn print_help() {
    println!(
        "\
usage-visualizer v0.1.0
Lightweight, ToS-compliant CLI usage dashboard for Claude Code, Codex, and Antigravity.

USAGE:
  usage-visualizer [COMMAND] [OPTIONS]

COMMANDS:
  watch          Persistent dashboard refreshing every 5 minutes (default)
  once           Fetch/render once and exit immediately
  setup-claude   Configure official Claude Code statusLine hook in ~/.claude/settings.json
  claude-hook    Internal stdin handler invoked by Claude Code's statusLine
  help           Show this help message

OPTIONS:
  -i, --interval <SECS>   Auto-refresh interval in seconds (default: 300)
  -1, --once              Run once and exit
  -r, --refresh           Bypass 5-minute cache TTL and fetch live data immediately
      --json              Print aggregated usage state as JSON and exit
      --warn <PERCENT>    Low remaining quota warning threshold (default: 20)
      --no-color          Disable ANSI terminal colors

ENVIRONMENT VARIABLES:
  CODEX_BIN                   Path to codex binary (auto-detected if omitted)
  AGY_BIN                     Path to agy binary (auto-detected if omitted)
  CLAUDE_CONFIG_DIR           Path to Claude config dir (default: ~/.claude)
  USAGE_VISUALIZER_CACHE_DIR  Path to cache directory (default: ~/.cache/usage-visualizer)
  TZ_OFFSET_HOURS             Local timezone offset from UTC in hours (default: 9)
"
    );
}

fn main() {
    let cfg = parse_args();

    match cfg.mode {
        RunMode::Help => {
            print_help();
        }
        RunMode::ClaudeHook => {
            let mut input = String::new();
            if io::stdin().read_to_string(&mut input).is_ok() && !input.trim().is_empty() {
                let now = current_epoch_secs();
                match process_statusline_payload(&input, &claude_cache_path(), now) {
                    Ok(bar) => {
                        print!("{}", bar);
                        let _ = io::stdout().flush();
                    }
                    Err(_) => {}
                }
            }
        }
        RunMode::SetupClaude => {
            let exe = env::current_exe().unwrap_or_else(|_| PathBuf::from("usage-visualizer"));
            let cmd = format!("{} claude-hook", exe.display());
            let settings_path = claude_settings_path();
            match configure_claude_settings(&settings_path, &cmd) {
                Ok(_) => {
                    println!(
                        "Configured Claude Code statusLine hook in {}",
                        settings_path.display()
                    );
                    println!("Hook command: {}", cmd);
                }
                Err(e) => {
                    eprintln!("Error configuring Claude Code statusLine: {}", e);
                    std::process::exit(1);
                }
            }
        }
        RunMode::Once => {
            let state = get_or_refresh_state(cfg.interval_secs, cfg.force_refresh);
            if cfg.json_output {
                println!("{}", state.to_json().to_string_pretty());
            } else {
                let now = current_epoch_secs();
                let opts = RenderOptions {
                    use_color: !cfg.no_color,
                    tz_offset_secs: cfg.tz_offset_secs,
                    watch_interval_secs: None,
                    warn_threshold_percent: cfg.warn_threshold,
                };
                print!("{}", render_dashboard(&state, now, &opts));
            }
        }
        RunMode::Watch => {
            let opts = RenderOptions {
                use_color: !cfg.no_color,
                tz_offset_secs: cfg.tz_offset_secs,
                watch_interval_secs: Some(cfg.interval_secs),
                warn_threshold_percent: cfg.warn_threshold,
            };

            let claude_path = claude_cache_path();
            let mut last_claude_mtime = fs::metadata(&claude_path)
                .and_then(|m| m.modified())
                .ok();

            let mut state = get_or_refresh_state(cfg.interval_secs, cfg.force_refresh);
            let now = current_epoch_secs();
            print!("\x1b[2J\x1b[H");
            print!("{}", render_dashboard(&state, now, &opts));
            println!("Press Ctrl+C to exit.");
            let _ = io::stdout().flush();

            loop {
                thread::sleep(Duration::from_secs(3));
                let now = current_epoch_secs();
                let mut needs_redraw = false;

                // 1. Check if Claude Code statusLine hook updated its cache file (zero subprocesses, 1 stat syscall)
                let cur_claude_mtime = fs::metadata(&claude_path)
                    .and_then(|m| m.modified())
                    .ok();
                if cur_claude_mtime != last_claude_mtime {
                    last_claude_mtime = cur_claude_mtime;
                    let fresh_claude = read_claude_cache(&claude_path, now);
                    if let Some(slot) = state
                        .providers
                        .iter_mut()
                        .find(|p| p.provider == "Claude Code")
                    {
                        *slot = fresh_claude;
                    }
                    save_cached_state(&state);
                    needs_redraw = true;
                }

                // 2. Check if the 5-minute refresh interval has elapsed
                if now >= state.updated_at_epoch + (cfg.interval_secs as i64) {
                    state = collect_all_providers(Some(&state));
                    needs_redraw = true;
                }

                if needs_redraw {
                    print!("\x1b[2J\x1b[H");
                    print!("{}", render_dashboard(&state, now, &opts));
                    println!("Press Ctrl+C to exit.");
                    let _ = io::stdout().flush();
                }
            }
        }
    }
}
