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

    tauri_build::build()
}
