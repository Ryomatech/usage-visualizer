use crate::model::{
    format_local_datetime, format_reset_countdown, DashboardState, ProviderUsage, QuotaWindow,
};

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const CYAN: &str = "\x1b[36m";

pub struct RenderOptions {
    pub use_color: bool,
    pub tz_offset_secs: i64,
    pub watch_interval_secs: Option<u64>,
    pub warn_threshold_percent: f64,
}

pub fn render_dashboard(state: &DashboardState, now_epoch: i64, opts: &RenderOptions) -> String {
    let mut out = String::new();
    let c = |code: &'static str| -> &'static str {
        if opts.use_color {
            code
        } else {
            ""
        }
    };

    let updated_str = format_local_datetime(state.updated_at_epoch, opts.tz_offset_secs, false);
    let mode_str = match opts.watch_interval_secs {
        Some(secs) => {
            let next_str = format_local_datetime(
                state.updated_at_epoch + secs as i64,
                opts.tz_offset_secs,
                false,
            );
            format!("auto-refresh: {}s | next: {}", secs, next_str)
        }
        None => "mode: one-shot".to_string(),
    };

    out.push_str(&format!(
        "{}{}AI Usage Dashboard{}  {}(Last updated: {} | {}){}\n",
        c(BOLD),
        c(CYAN),
        c(RESET),
        c(DIM),
        updated_str,
        mode_str,
        c(RESET)
    ));
    out.push_str(&format!("{}{}\n", c(DIM), "─".repeat(88)));
    out.push_str(&format!(
        "{}{:<15} {:<25} {:>9}   {:<22} {:<20}{}\n",
        c(BOLD),
        "Provider",
        "Window",
        "Remaining",
        "Progress",
        "Resets In",
        c(RESET)
    ));
    out.push_str(&format!("{}{}{}\n", c(DIM), "─".repeat(88), c(RESET)));

    let mut warnings: Vec<String> = Vec::new();

    for (idx, provider) in state.providers.iter().enumerate() {
        if idx > 0 {
            out.push_str(&format!("{}{}{}\n", c(DIM), "·".repeat(88), c(RESET)));
        }
        render_provider(provider, now_epoch, opts, &mut out, &mut warnings);
    }

    out.push_str(&format!("{}{}{}\n", c(DIM), "─".repeat(88), c(RESET)));

    if !warnings.is_empty() {
        for w in &warnings {
            out.push_str(&format!("{}{}⚠ {}{}\n", c(BOLD), c(RED), w, c(RESET)));
        }
    }

    out
}

fn render_provider(
    provider: &ProviderUsage,
    now_epoch: i64,
    opts: &RenderOptions,
    out: &mut String,
    warnings: &mut Vec<String>,
) {
    let c = |code: &'static str| -> &'static str {
        if opts.use_color {
            code
        } else {
            ""
        }
    };

    if provider.windows.is_empty() {
        let status_color = if provider.status == "waiting" {
            c(YELLOW)
        } else {
            c(RED)
        };
        let msg = provider
            .error_message
            .as_deref()
            .unwrap_or("No usage windows available");
        out.push_str(&format!(
            "{}{:<15}{} {}{}{}\n",
            c(BOLD),
            provider.provider,
            c(RESET),
            status_color,
            msg,
            c(RESET)
        ));
        return;
    }

    for (row_idx, raw_win) in provider.windows.iter().enumerate() {
        let win: QuotaWindow = raw_win.normalized_for_now(now_epoch);
        let provider_col = if row_idx == 0 {
            provider.provider.as_str()
        } else {
            ""
        };

        let rem = win.remaining_percent;
        let bar_color = if rem < opts.warn_threshold_percent {
            c(RED)
        } else if rem < 50.0 {
            c(YELLOW)
        } else {
            c(GREEN)
        };

        if rem < opts.warn_threshold_percent {
            warnings.push(format!(
                "{} [{}] is low: {:.0}% remaining",
                provider.provider, win.label, rem
            ));
        }

        let bar = build_progress_bar(rem, 20);
        let pct_num = if (rem - rem.round()).abs() < 0.05 {
            format!("{:>5.0}%", rem)
        } else {
            format!("{:>5.1}%", rem)
        };
        let rem_str = if rem < opts.warn_threshold_percent {
            format!("{} (!)", pct_num)
        } else {
            format!("{}    ", pct_num)
        };
        let reset_str =
            format_reset_countdown(win.resets_at_epoch, now_epoch, opts.tz_offset_secs);

        out.push_str(&format!(
            "{}{:<15}{} {:<25} {}{}{}{} {}{}{} {:<20}\n",
            c(BOLD),
            provider_col,
            c(RESET),
            win.label,
            bar_color,
            c(BOLD),
            rem_str,
            c(RESET),
            bar_color,
            bar,
            c(RESET),
            reset_str
        ));
    }

    // Optional metadata line (plan, reset credits, source)
    let mut meta_parts = Vec::new();
    if let Some(plan) = &provider.plan {
        meta_parts.push(format!("plan: {}", plan));
    }
    if let Some(note) = &provider.extra_note {
        meta_parts.push(note.clone());
    }
    meta_parts.push(format!("source: {}", provider.source));
    if provider.status == "cached" {
        meta_parts.push("cached".to_string());
    }

    out.push_str(&format!(
        "{:<15} {}{}{}\n",
        "",
        c(DIM),
        meta_parts.join(" | "),
        c(RESET)
    ));
}

fn build_progress_bar(remaining_percent: f64, width: usize) -> String {
    let clamped = remaining_percent.clamp(0.0, 100.0);
    let filled = ((clamped / 100.0) * (width as f64)).round() as usize;
    let filled = filled.min(width);
    let empty = width - filled;
    format!("[{}{}]", "█".repeat(filled), "░".repeat(empty))
}
