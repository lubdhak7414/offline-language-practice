fn main() {
    // Re-run the build script when the Tauri config changes so bundled
    // metadata/resources stay in sync.
    println!("cargo:rerun-if-changed=tauri.conf.json");

    // Models are not bundled (`bundle.resources` ships only
    // `resources/espeak-ng-data/`); installs fetch them into the app-data dir.
    // `$RESOURCE/models/` stays a search root in `paths`, so a packager may
    // still drop weights there. Creating the gitignored repo-root `models/`
    // is harmless and keeps `scripts/download-models.sh`'s target present.
    let _ = std::fs::create_dir_all("../models");
    println!("cargo:rerun-if-changed=../models");

    tauri_build::build()
}
