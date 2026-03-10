//! Central directory resolution for smolvm.
//!
//! When `SMOLVM_HOME` is set, all paths are rooted under that single directory.
//! Otherwise, platform defaults (via the `dirs` crate) are used, which mirrors
//! the historical behaviour:
//!
//! | Platform | data_dir                              | cache_dir                    | runtime_dir              |
//! |----------|---------------------------------------|------------------------------|--------------------------|
//! | macOS    | `~/Library/Application Support/smolvm`| `~/Library/Caches/smolvm`    | `~/Library/Caches/smolvm`|
//! | Linux    | `~/.local/share/smolvm`               | `~/.cache/smolvm`            | `$XDG_RUNTIME_DIR/smolvm`|
//!
//! When `SMOLVM_HOME=/path/to/dir` the layout becomes:
//!
//! ```text
//! $SMOLVM_HOME/          ← data_dir()
//! $SMOLVM_HOME/cache/    ← cache_dir()
//! $SMOLVM_HOME/run/      ← runtime_dir()
//! ```
//!
//! This allows an installer (e.g. Zota) to set `SMOLVM_HOME=~/.zota/data/smolvm`
//! and have everything live under `~/.zota/` instead of being scattered across
//! several platform-specific directories.

use std::path::PathBuf;

/// Root data directory for smolvm.
///
/// - If `SMOLVM_HOME` is set: `$SMOLVM_HOME`
/// - macOS default: `~/Library/Application Support/smolvm`
/// - Linux default: `~/.local/share/smolvm`
///
/// # Examples
///
/// ```no_run
/// // With SMOLVM_HOME=/tmp/mysmolvm:
/// // Returns Some("/tmp/mysmolvm")
/// let dir = smolvm::paths::data_dir();
/// ```
pub fn data_dir() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("SMOLVM_HOME") {
        return Some(PathBuf::from(home));
    }
    dirs::data_local_dir()
        .or_else(dirs::data_dir)
        .map(|d| d.join("smolvm"))
}

/// Cache directory for smolvm (named VMs, runtime state).
///
/// - If `SMOLVM_HOME` is set: `$SMOLVM_HOME/cache`
/// - macOS default: `~/Library/Caches/smolvm`
/// - Linux default: `~/.cache/smolvm`
///
/// # Examples
///
/// ```no_run
/// // With SMOLVM_HOME=/tmp/mysmolvm:
/// // Returns Some("/tmp/mysmolvm/cache")
/// let dir = smolvm::paths::cache_dir();
/// ```
pub fn cache_dir() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("SMOLVM_HOME") {
        return Some(PathBuf::from(home).join("cache"));
    }
    dirs::cache_dir().map(|d| d.join("smolvm"))
}

/// Runtime directory for smolvm (sockets, PID files, logs).
///
/// - If `SMOLVM_HOME` is set: `$SMOLVM_HOME/run`
/// - Linux default: `$XDG_RUNTIME_DIR/smolvm`
/// - macOS / fallback: `~/Library/Caches/smolvm` (or `/tmp/smolvm`)
///
/// # Examples
///
/// ```no_run
/// // With SMOLVM_HOME=/tmp/mysmolvm:
/// // Returns "/tmp/mysmolvm/run"
/// let dir = smolvm::paths::runtime_dir();
/// ```
pub fn runtime_dir() -> PathBuf {
    if let Ok(home) = std::env::var("SMOLVM_HOME") {
        return PathBuf::from(home).join("run");
    }
    dirs::runtime_dir()
        .or_else(dirs::cache_dir)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("smolvm")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_smolvm_home_overrides_data_dir() {
        // Safety: env-var mutation in tests is inherently racy when tests run in
        // parallel. These tests are self-contained and do not touch the filesystem.
        std::env::set_var("SMOLVM_HOME", "/test/home");
        assert_eq!(data_dir(), Some(PathBuf::from("/test/home")));
        std::env::remove_var("SMOLVM_HOME");
    }

    #[test]
    fn test_smolvm_home_overrides_cache_dir() {
        std::env::set_var("SMOLVM_HOME", "/test/home");
        assert_eq!(cache_dir(), Some(PathBuf::from("/test/home/cache")));
        std::env::remove_var("SMOLVM_HOME");
    }

    #[test]
    fn test_smolvm_home_overrides_runtime_dir() {
        std::env::set_var("SMOLVM_HOME", "/test/home");
        assert_eq!(runtime_dir(), PathBuf::from("/test/home/run"));
        std::env::remove_var("SMOLVM_HOME");
    }

    #[test]
    fn test_default_dirs_contain_smolvm() {
        std::env::remove_var("SMOLVM_HOME");
        if let Some(d) = data_dir() {
            assert!(d.to_string_lossy().contains("smolvm"));
        }
        if let Some(d) = cache_dir() {
            assert!(d.to_string_lossy().contains("smolvm"));
        }
        assert!(runtime_dir().to_string_lossy().contains("smolvm"));
    }
}
