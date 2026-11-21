pub mod antigravity;
pub mod claude;
pub mod codex;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

pub fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn cache_dir() -> PathBuf {
    if let Some(custom) = env::var_os("USAGE_VISUALIZER_CACHE_DIR") {
        return PathBuf::from(custom);
    }
    if let Some(xdg) = env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(xdg).join("usage-visualizer");
    }
    home_dir().join(".cache").join("usage-visualizer")
}

pub fn resolve_binary(bin_name: &str, env_override: &str) -> PathBuf {
    if let Some(custom) = env::var_os(env_override) {
        let p = PathBuf::from(custom);
        if !p.as_os_str().is_empty() {
            return p;
        }
    }

    if let Some(path_var) = env::var_os("PATH") {
        for dir in env::split_paths(&path_var) {
            let candidate = dir.join(bin_name);
            if candidate.exists() {
                return candidate;
            }
        }
    }

    let home = home_dir();
    let fallbacks = [
        home.join(".local").join("bin").join(bin_name),
        PathBuf::from("/opt/homebrew/bin").join(bin_name),
        PathBuf::from("/usr/local/bin").join(bin_name),
    ];
    for candidate in &fallbacks {
        if candidate.exists() {
            return candidate.clone();
        }
    }

    PathBuf::from(bin_name)
}

pub fn write_private_file(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create directory {}: {}", parent.display(), e))?;
    }
    let tmp_path = path.with_extension("tmp");
    fs::write(&tmp_path, content)
        .map_err(|e| format!("Failed to write temp file {}: {}", tmp_path.display(), e))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o600));
    }

    fs::rename(&tmp_path, path).map_err(|e| {
        format!(
            "Failed to rename {} to {}: {}",
            tmp_path.display(),
            path.display(),
            e
        )
    })
}
