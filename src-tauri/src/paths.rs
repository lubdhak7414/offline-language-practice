//! Extra model search roots, resolved once at startup.
//!
//! On-device weights live outside the binary: dev trees use CWD-relative
//! `models/` dirs; installed apps download them into the app-data dir's
//! `models/`, and `$RESOURCE/models/` is also searched in case a packager
//! ships weights there (the bundle itself does not). The neural workers run
//! on threads without an app handle, so `run()` snapshots the resolved dirs
//! here and the `asr`/`tts` loaders consult them after their CWD candidates.

use std::path::{Path, PathBuf};
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

/// The env var `espeak-rs` reads for the directory that *contains*
/// `espeak-ng-data`.
pub const ESPEAK_DATA_ENV: &str = "PIPER_ESPEAKNG_DATA_DIRECTORY";

/// Where espeak should look for its data, if `run()` must say so.
///
/// `espeak-rs` tries this env var, then the CWD, then the exe dir, and
/// otherwise hands espeak a null path, which makes espeak fall back to the
/// absolute `OUT_DIR` path compiled into the binary — a directory that only
/// exists on the machine that built it. Installed bundles keep the data in
/// the resource dir instead (`bundle.resources`), so point espeak there.
/// An explicit env var is left alone; a resource dir without the data (a
/// `cargo run` tree) returns `None` and keeps today's fallback.
pub fn espeak_data_parent(resource_dir: Option<&Path>, env_set: bool) -> Option<PathBuf> {
    if env_set {
        return None;
    }
    let dir = resource_dir?;
    dir.join("espeak-ng-data")
        .join("phontab")
        .is_file()
        .then(|| dir.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn espeak_data_is_pointed_at_the_resource_dir_only_when_it_holds_the_data() {
        let root = std::env::temp_dir().join("olp-paths-espeak-test-31337");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // No data yet: keep espeak-rs's own search.
        assert_eq!(espeak_data_parent(Some(&root), false), None);
        std::fs::create_dir_all(root.join("espeak-ng-data")).unwrap();
        std::fs::write(root.join("espeak-ng-data").join("phontab"), b"x").unwrap();
        assert_eq!(espeak_data_parent(Some(&root), false), Some(root.clone()));
        // The user's own override wins.
        assert_eq!(espeak_data_parent(Some(&root), true), None);
        assert_eq!(espeak_data_parent(None, false), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_vendored_espeak_data_is_complete() {
        // The files `scripts/vendor-espeak-data.sh` copies. Missing any one
        // of them makes espeak_Initialize fail on a user's machine, where
        // there is no build tree to fall back to.
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources");
        assert_eq!(espeak_data_parent(Some(&dir), false), Some(dir.clone()));
        for f in [
            "phondata",
            "phonindex",
            "phontab",
            "intonations",
            "en_dict",
            "lang/gmw/en-US",
        ] {
            let p = dir.join("espeak-ng-data").join(f);
            assert!(p.is_file(), "missing {}", p.display());
        }
    }

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
