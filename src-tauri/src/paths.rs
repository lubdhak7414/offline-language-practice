//! Extra model search roots, resolved once at startup.
//!
//! On-device weights live outside the binary: dev trees use CWD-relative
//! `models/` dirs, while installed bundles ship them under Tauri's resource
//! dir (`$RESOURCE/models/` via `bundle.resources`). The neural workers run
//! on threads without an app handle, so `run()` snapshots the resolved dirs
//! here and the `asr`/`tts` loaders consult them after their CWD candidates.

use std::path::PathBuf;
use std::sync::OnceLock;

static EXTRA: OnceLock<Vec<PathBuf>> = OnceLock::new();

/// Snapshot extra search dirs. First call wins; later calls are ignored.
pub fn init(dirs: Vec<PathBuf>) {
    let _ = EXTRA.set(dirs);
}

/// Extra roots: `OLP_MODELS_DIR` env override first (dev/debugging),
/// then the startup-registered dirs (resource dir, …).
pub fn extra_model_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(dir) = std::env::var_os("OLP_MODELS_DIR") {
        let p = PathBuf::from(dir);
        if !out.contains(&p) {
            out.push(p);
        }
    }
    if let Some(dirs) = EXTRA.get() {
        for d in dirs {
            if !out.contains(d) {
                out.push(d.clone());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_override_is_first_root() {
        let dir = std::env::temp_dir().join("olp-paths-test-unique-84521");
        std::env::set_var("OLP_MODELS_DIR", &dir);
        let roots = extra_model_dirs();
        std::env::remove_var("OLP_MODELS_DIR");
        assert!(
            roots.first() == Some(&dir),
            "env override should be first root"
        );
    }
}
