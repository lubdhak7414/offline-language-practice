//! Self-update (Phase 7). The only network this module causes is a GET of
//! `latest.json` from this repository's GitHub Releases, and — only when the
//! user presses Install — the one signed bundle it names.
//!
//! Three rules:
//! 1. Consent lives here, not in the webview: an automatic check with the
//!    preference off is refused before any request is built.
//! 2. Only bundles the app can replace itself are installed. A .deb/.rpm is
//!    owned by the package manager; the plugin would drive pkexec/zenity/sudo
//!    for it, which this app will not do.
//! 3. The endpoint is fixed. `OLP_UPDATE_ENDPOINT` may point at another
//!    release of *this* repository (rehearsals), nothing else.
//!
//! The webview holds no `updater:*` permission: the plugin's own commands
//! are unreachable from JS, so these rules cannot be bypassed from there.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tauri::utils::config::BundleType;

pub const RELEASES: &str = "https://github.com/lubdhak7414/offline-language-practice/releases";

/// Who asked for a check. Crosses IPC as `"manual"` / `"startup"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Trigger {
    Manual,
    Startup,
}

/// A click on "Check now" is consent; a launch is not.
pub fn may_check(trigger: Trigger, opted_in: bool) -> bool {
    trigger == Trigger::Manual || opted_in
}

/// Whether this copy may replace itself, and if not, what to tell the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Install,
    Notify(&'static str),
}

pub const PACKAGE_REASON: &str = "This copy was installed from a Linux package, so your package \
     manager owns it. Install the new version the same way you installed this one.";
pub const UNBUNDLED_REASON: &str =
    "This copy was not installed from a release, so it cannot replace itself.";

/// `bundle` is `tauri::utils::platform::bundle_type()`: the bundler patches
/// the type into the binary, so a raw `cargo build` reports `None` on Linux
/// and Windows. On macOS it always reports `App`, bundled or not.
pub fn install_mode(os: &str, bundle: Option<BundleType>) -> Mode {
    match (os, bundle) {
        ("linux", Some(BundleType::AppImage))
        | ("macos", Some(BundleType::App))
        | ("windows", Some(BundleType::Msi | BundleType::Nsis)) => Mode::Install,
        ("linux", Some(BundleType::Deb | BundleType::Rpm)) => Mode::Notify(PACKAGE_REASON),
        _ => Mode::Notify(UNBUNDLED_REASON),
    }
}

/// The name Settings shows under "Installed as". `None` = development build.
pub fn bundle_name(bundle: Option<BundleType>) -> Option<&'static str> {
    Some(match bundle? {
        BundleType::AppImage => "AppImage",
        BundleType::Deb => "deb",
        BundleType::Rpm => "rpm",
        BundleType::App => "app",
        BundleType::Msi => "msi",
        BundleType::Nsis => "nsis",
        _ => return None,
    })
}

/// `OLP_UPDATE_ENDPOINT`, if set, must be
/// `{RELEASES}/download/v<digits.dots-dashes>/latest.json` and nothing else.
/// Even then it can only select a manifest whose bundles this repository's
/// key signed, and the plugin's "must be newer" rule still applies.
pub fn endpoint_override(raw: Option<&str>) -> Result<Option<String>, String> {
    let Some(raw) = raw else { return Ok(None) };
    let bad = || format!("OLP_UPDATE_ENDPOINT must be {RELEASES}/download/v<version>/latest.json");
    let prefix = format!("{RELEASES}/download/");
    let rest = raw.strip_prefix(prefix.as_str()).ok_or_else(bad)?;
    let (tag, file) = rest.split_once('/').ok_or_else(bad)?;
    let version = tag.strip_prefix('v').ok_or_else(bad)?;
    let tag_ok = !version.is_empty()
        && version
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == '-');
    if !tag_ok || file != "latest.json" {
        return Err(bad());
    }
    Ok(Some(raw.to_string()))
}

/// Install progress, sent over the command's `Channel`. Mirrored by the TS
/// `UpdateEvent` union (tag `kind`).
#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum UpdateEvent {
    Started { total: Option<u64> },
    Progress { received: u64, total: Option<u64> },
    Verifying,
    Installing,
    Installed,
}

/// One check-or-install at a time; `pending` is the last check's answer.
/// `Update` is `Send + Sync` (the plugin stores it as a tauri `Resource`).
#[derive(Default)]
pub struct UpdateState {
    pub pending: Mutex<Option<tauri_plugin_updater::Update>>,
    busy: AtomicBool,
    installed: AtomicBool,
}

/// Held for the length of a check or install; clears `busy` when dropped,
/// on every exit path including an error.
pub struct Busy<'a>(&'a UpdateState);

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::SeqCst);
    }
}

impl UpdateState {
    pub fn begin(&self) -> Option<Busy<'_>> {
        self.busy
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| Busy(self))
    }

    pub fn mark_installed(&self) {
        self.installed.store(true, Ordering::SeqCst);
    }

    pub fn is_installed(&self) -> bool {
        self.installed.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_mode_by_bundle() {
        use BundleType::*;
        for (os, bundle) in [
            ("linux", AppImage),
            ("macos", App),
            ("windows", Msi),
            ("windows", Nsis),
        ] {
            assert_eq!(
                install_mode(os, Some(bundle.clone())),
                Mode::Install,
                "{os} {bundle:?}"
            );
        }
        for bundle in [Deb, Rpm] {
            assert_eq!(
                install_mode("linux", Some(bundle)),
                Mode::Notify(PACKAGE_REASON)
            );
        }
        for os in ["linux", "macos", "windows"] {
            assert_eq!(
                install_mode(os, None),
                Mode::Notify(UNBUNDLED_REASON),
                "{os}"
            );
        }
        // A bundle type on the wrong OS is not something to install over.
        assert_eq!(
            install_mode("linux", Some(Msi)),
            Mode::Notify(UNBUNDLED_REASON)
        );
        assert_eq!(
            install_mode("macos", Some(Dmg)),
            Mode::Notify(UNBUNDLED_REASON)
        );
        assert_eq!(bundle_name(Some(AppImage)), Some("AppImage"));
        assert_eq!(bundle_name(Some(Dmg)), None);
        assert_eq!(bundle_name(None), None);
    }

    #[test]
    fn automatic_checks_need_consent() {
        assert!(!may_check(Trigger::Startup, false));
        assert!(may_check(Trigger::Startup, true));
        assert!(may_check(Trigger::Manual, false));
        assert!(may_check(Trigger::Manual, true));
        // The wire names the frontend sends.
        let t: Trigger = serde_json::from_str("\"startup\"").unwrap();
        assert_eq!(t, Trigger::Startup);
        assert!(serde_json::from_str::<Trigger>("\"always\"").is_err());
    }

    #[test]
    fn endpoint_override_accepts_only_this_repos_release_manifests() {
        let ok = format!("{RELEASES}/download/v0.1.0-12/latest.json");
        assert_eq!(endpoint_override(Some(&ok)), Ok(Some(ok.clone())));
        assert_eq!(endpoint_override(None), Ok(None));
        for bad in [
            "http://github.com/lubdhak7414/offline-language-practice/releases/download/v0.1.0/latest.json",
            "https://github.com/someone-else/offline-language-practice/releases/download/v0.1.0/latest.json",
            "https://github.com/lubdhak7414/other-repo/releases/download/v0.1.0/latest.json",
            "https://github.com.evil.com/lubdhak7414/offline-language-practice/releases/download/v0.1.0/latest.json",
            "https://github.com/lubdhak7414/offline-language-practice/releases/latest/download/latest.json",
            "https://github.com/lubdhak7414/offline-language-practice/releases/download/v0.1.0/../x/latest.json",
            "https://github.com/lubdhak7414/offline-language-practice/releases/download/v%2e%2e/latest.json",
            "https://github.com/lubdhak7414/offline-language-practice/releases/download/v1?x=1",
            "https://github.com/lubdhak7414/offline-language-practice/releases/download/v1/latest.json?x=1",
            "https://github.com/lubdhak7414/offline-language-practice/releases/download/v0.1.0/latest.json.sig",
            "https://github.com/lubdhak7414/offline-language-practice/releases/download/v/latest.json",
            "https://github.com/lubdhak7414/offline-language-practice/releases/download/0.1.0/latest.json",
            "",
        ] {
            assert!(endpoint_override(Some(bad)).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn update_state_is_a_singleton() {
        let s = UpdateState::default();
        let first = s.begin();
        assert!(first.is_some());
        assert!(s.begin().is_none(), "a second check must be refused");
        drop(first);
        assert!(s.begin().is_some(), "dropping the guard frees the slot");
        assert!(!s.is_installed());
        s.mark_installed();
        assert!(s.is_installed());
    }

    #[test]
    fn update_event_wire_shape() {
        assert_eq!(
            serde_json::to_value(UpdateEvent::Verifying).unwrap(),
            serde_json::json!({ "kind": "verifying" })
        );
        assert_eq!(
            serde_json::to_value(UpdateEvent::Progress {
                received: 5,
                total: Some(10)
            })
            .unwrap(),
            serde_json::json!({ "kind": "progress", "received": 5, "total": 10 })
        );
        assert_eq!(
            serde_json::to_value(UpdateEvent::Started { total: None }).unwrap(),
            serde_json::json!({ "kind": "started", "total": null })
        );
    }
}
