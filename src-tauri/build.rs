fn main() {
    // Re-run the build script when the Tauri config changes so bundled
    // metadata/resources stay in sync.
    println!("cargo:rerun-if-changed=tauri.conf.json");

    // The `bundle.resources` map references `../models/` (repo-root models/
    // populated by scripts/download-models.sh). Tauri's build script fails
    // the whole build when a resource pattern matches nothing, so ensure the
    // directory exists: an empty dir is silently skipped by the resource
    // iterator, while a populated one gets bundled to `$RESOURCE/models/`.
    // `models/` is gitignored build input, so creating it here is harmless.
    // The build script cwd is the package dir (src-tauri/), hence `../models`.
    let _ = std::fs::create_dir_all("../models");
    println!("cargo:rerun-if-changed=../models");

    // Bake the short git SHA into the binary for release diagnostics.
    // Falls back to "unknown" when git is absent (e.g. source tarball with
    // no .git dir) so the build never fails for this.
    let sha = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=APP_GIT_SHA={sha}");

    tauri_build::build()
}
