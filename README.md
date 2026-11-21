# usage-visualizer

A lightweight, zero-dependency Rust CLI tool that visualizes remaining usage quotas and reset times for **Claude Code**, **Codex**, and **Antigravity** in the terminal.

## Key Features

- **Persistent Auto-Refresh Dashboard (`watch` mode by default)**: Updates automatically every 5 minutes (`300s`, configurable via `--interval`) without manual re-execution.
- **Minimal CPU & Memory Footprint**:
  - Compiled native Rust binary with **zero external crate dependencies** (`std` only).
  - Child processes (`codex app-server`, `agy -p /usage`) are spawned **only during a refresh** and immediately terminated and reaped (`kill()` + `wait()`). Zero helper processes remain in memory during the 5-minute sleep interval.
  - Uses ~2 MB RSS in watch mode and 0.0% CPU while sleeping between refreshes.
- **Terms of Service (ToS) Compliant by Design**:
  - **Claude Code**: Uses the official `claude` CLI stream-json `get_usage` control request (`skip_behaviors: true`, 0 tokens consumed) and/or the official `statusLine` JSON hook (`rate_limits.five_hour` and `rate_limits.seven_day`). Never calls undocumented OAuth endpoints (`/api/oauth/usage`). Automatically resets cached usage to 100% remaining once `resets_at` passes.
  - **Codex**: Uses the official `codex app-server` JSON-RPC protocol (`account/rateLimits/read`) documented by OpenAI.
  - **Antigravity**: Uses the official `agy -p "/usage" --output-format json` CLI command documented by Google (consumes 0 tokens and requires no undocumented IDE port/CSRF scraping).

## Quick Start

### 1. Build

```bash
cargo build --release
```

The binary is created at `./target/release/usage-visualizer`.

### 2. Prerequisites (Authenticate CLIs Once)

Make sure each CLI tool you want to monitor is logged in:

```bash
claude auth login
codex login
agy
```

*(Optional)* You can also enable the passive Claude Code `statusLine` hook so terminal `claude` sessions update the cache instantaneously on every turn:

```bash
./target/release/usage-visualizer setup-claude
```

### 3. Run the Dashboard

```bash
# Default: persistent watch mode (auto-refreshes every 5 minutes)
./target/release/usage-visualizer

# Custom refresh interval (e.g., every 180 seconds)
./target/release/usage-visualizer --interval 180

# One-shot check (uses 5-minute cache if fresh)
./target/release/usage-visualizer --once

# Force live refresh in one-shot mode
./target/release/usage-visualizer --once --refresh

# Structured JSON output for scripts / tmux
./target/release/usage-visualizer --json
```

## Environment Variables

| Variable | Description | Default |
| :--- | :--- | :--- |
| `CODEX_BIN` | Path to `codex` binary | Auto-detected via `PATH` / `~/.local/bin/codex` |
| `AGY_BIN` | Path to `agy` binary | Auto-detected via `PATH` / `~/.local/bin/agy` |
| `CLAUDE_CONFIG_DIR` | Claude Code config directory | `~/.claude` |
| `USAGE_VISUALIZER_CACHE_DIR` | Cache directory for snapshots | `~/.cache/usage-visualizer` |
| `TZ_OFFSET_HOURS` | Local timezone offset from UTC | `9` (JST, UTC+9) |
| `NO_COLOR` | Disable ANSI terminal colors | Unset |

## License

MIT
