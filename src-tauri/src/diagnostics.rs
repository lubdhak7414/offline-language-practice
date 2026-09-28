//! A text report a user can attach to a bug report.
//!
//! It says what the app is and what state it is in, and nothing about what
//! the user practised: no attempts, transcripts, audio, card text or
//! prompts, only counts. The microphone id is left out too, since it
//! identifies the hardware. `render` is pure so the redaction is testable.

use crate::prefs::Preferences;

/// Everything the report is built from.
pub struct Snapshot<'a> {
    pub version: &'a str,
    pub os: &'a str,
    pub arch: &'a str,
    /// How the app was installed (`AppImage`, `deb`, …), or `None` for a dev build.
    pub bundle: Option<&'a str>,
    pub asr_model: bool,
    pub asr_vocab: bool,
    pub tts_voice: bool,
    pub ep_report: &'a str,
    pub prefs: &'a Preferences,
    pub cards: i64,
    pub decks: i64,
    pub reviews: i64,
    pub attempts: i64,
    pub custom_prompts: i64,
}

fn yes_no(b: bool) -> &'static str {
    if b {
        "installed"
    } else {
        "missing"
    }
}

/// The report text. Preferences are listed by name; free-text and device
/// identifiers are not.
pub fn render(s: &Snapshot<'_>) -> String {
    let p = s.prefs;
    let mut out = String::new();
    let mut line = |text: String| {
        out.push_str(&text);
        out.push('\n');
    };
    line("Offline Language Practice — diagnostics".to_string());
    line(String::new());
    line(format!("version: {}", s.version));
    line(format!("platform: {} {}", s.os, s.arch));
    line(format!(
        "installed as: {}",
        s.bundle.unwrap_or("development build")
    ));
    line(String::new());
    line("models".to_string());
    line(format!(
        "  speech recognition model: {}",
        yes_no(s.asr_model)
    ));
    line(format!(
        "  speech recognition vocabulary: {}",
        yes_no(s.asr_vocab)
    ));
    line(format!("  voice: {}", yes_no(s.tts_voice)));
    line(String::new());
    line("execution providers".to_string());
    for l in s.ep_report.lines() {
        line(format!("  {l}"));
    }
    line(String::new());
    line("preferences".to_string());
    line(format!("  dialect: {}", p.dialect));
    line(format!("  theme: {}", p.theme));
    line(format!("  goal: {}", p.goal));
    line(format!("  day cutoff hour: {}", p.day_cutoff_hour));
    line(format!("  new per day: {}", p.new_per_day));
    line(format!("  reviews per day: {}", p.review_per_day));
    line(format!("  bury hours: {}", p.bury_hours));
    line(format!(
        "  daily speaking goal: {}",
        p.practice_goal_attempts
    ));
    line(format!(
        "  update checks: {}",
        if p.check_updates { "on" } else { "off" }
    ));
    line(format!("  onboarded: {}", p.onboarded));
    line(format!(
        "  microphone: {}",
        if p.mic_device_id.is_empty() {
            "system default"
        } else {
            "chosen"
        }
    ));
    line(String::new());
    line("data (counts only)".to_string());
    line(format!("  decks: {}", s.decks));
    line(format!("  cards: {}", s.cards));
    line(format!("  reviews: {}", s.reviews));
    line(format!("  practice attempts: {}", s.attempts));
    line(format!("  your own prompts: {}", s.custom_prompts));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot<'a>(prefs: &'a Preferences, ep: &'a str) -> Snapshot<'a> {
        Snapshot {
            version: "0.1.0",
            os: "linux",
            arch: "x86_64",
            bundle: Some("AppImage"),
            asr_model: true,
            asr_vocab: true,
            tts_voice: false,
            ep_report: ep,
            prefs,
            cards: 12,
            decks: 2,
            reviews: 340,
            attempts: 41,
            custom_prompts: 3,
        }
    }

    #[test]
    fn the_report_states_what_the_app_is_and_how_it_is_doing() {
        let prefs = Preferences::default();
        let text = render(&snapshot(&prefs, "CPU\nCUDA unavailable"));
        assert!(text.contains("version: 0.1.0"));
        assert!(text.contains("platform: linux x86_64"));
        assert!(text.contains("installed as: AppImage"));
        assert!(text.contains("voice: missing"));
        assert!(text.contains("  CUDA unavailable"));
        assert!(text.contains("practice attempts: 41"));
        assert!(text.contains("update checks: off"));
    }

    #[test]
    fn a_development_build_says_so() {
        let prefs = Preferences::default();
        let mut s = snapshot(&prefs, "");
        s.bundle = None;
        assert!(render(&s).contains("installed as: development build"));
    }

    #[test]
    fn nothing_identifying_or_personal_is_written() {
        let prefs = Preferences {
            mic_device_id: "hw:CARD=SecretHeadset,DEV=0".to_string(),
            tts_voice: "en_US-private-voice".to_string(),
            ..Preferences::default()
        };
        let text = render(&snapshot(&prefs, "CPU"));
        assert!(
            !text.contains("SecretHeadset"),
            "device ids identify hardware"
        );
        assert!(!text.contains("private-voice"));
        assert!(text.contains("microphone: chosen"));
    }
}
