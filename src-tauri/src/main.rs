//! Binary entry — delegates to the library `run` (Tauri v2 mobile_entry_point pattern).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    offline_language_practice_lib::run();
}
