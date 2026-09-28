import { useCallback, useEffect, useRef, useState } from "preact/hooks";

import { listMics, type Mic } from "../app/devices";
import { createRecorder } from "../app/recorder";
import { ipc } from "../ipc/commands";
import { createClipPlayer, speak } from "../lib/audio/player";
import { friendlyMicError, friendlyTtsError } from "../lib/errors";
import { describeMicLevel } from "../lib/micLevel";
import { ModelDownloads, formatBytes } from "../components/ModelDownloads";
import type {
  AvailableUpdate,
  BackupInfo,
  ModelStatus,
  Preferences,
  ReviewStats,
  UpdateInfo,
  VoiceInfo,
} from "../ipc/types";
import { GOALS } from "../lib/goals";

const DIALECTS = ["american", "british", "canadian", "australian"] as const;
const THEMES = ["system", "light", "dark"] as const;

/** Where the Updates section is. One at a time, like the backend's `UpdateState`. */
type UpdateStatus =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "newest" }
  | { kind: "available"; update: AvailableUpdate }
  | { kind: "downloading"; version: string; received: number; total: number | null }
  | { kind: "verifying" }
  | { kind: "installing" }
  | { kind: "installed"; version: string }
  | { kind: "error"; message: string };

/** "Remember about N out of 100" reads better than a bare decimal retention. */
function retentionLabel(r: number): string {
  return `Remember about ${Math.round(r * 100)} out of 100`;
}

/** How long "Test microphone" listens. */
const MIC_TEST_MS = 2000;

export function Settings(props: { announce: (msg: string) => void }) {
  const { announce } = props;
  const [mics, setMics] = useState<Mic[]>([]);
  const [micResult, setMicResult] = useState<string | null>(null);
  const [micTesting, setMicTesting] = useState(false);
  const micTestingRef = useRef(false);
  const [voices, setVoices] = useState<VoiceInfo[]>([]);
  const [voice, setVoiceState] = useState("");
  const [prefs, setPrefsState] = useState<Preferences | null>(null);
  const [retention, setRetentionState] = useState(0.9);
  const [stats, setStats] = useState<ReviewStats | null>(null);
  const [modelStatus, setModelStatus] = useState<ModelStatus | null>(null);
  const [epReport, setEpReport] = useState<string>("");
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [restoreInfo, setRestoreInfo] = useState<BackupInfo | null>(null);
  const [updateInfo, setUpdateInfo] = useState<UpdateInfo | null>(null);
  const [updateStatus, setUpdateStatus] = useState<UpdateStatus>({ kind: "idle" });

  const optimizing = useRef(false);
  const restoring = useRef(false);
  const player = useRef(createClipPlayer());
  const speaking = useRef(false);
  // Both have a side effect (a request to github.com, an install), so a
  // second click is dropped, not queued. The backend refuses it too.
  const checkingUpdate = useRef(false);
  const installing = useRef(false);

  useEffect(() => {
    const clips = player.current;
    return () => clips.stop();
  }, []);

  useEffect(() => {
    void (async () => {
      try {
        const [v, id, p, r, s, ms, ep, ui] = await Promise.all([
          ipc().listVoices(),
          ipc().getVoice(),
          ipc().getPreferences(),
          ipc().getRetention(),
          ipc().reviewStats(),
          ipc().modelStatus(),
          ipc().epReport(),
          ipc().updateInfo(),
        ]);
        setVoices(v);
        setVoiceState(id);
        setPrefsState(p);
        setRetentionState(r);
        setStats(s);
        setModelStatus(ms);
        setEpReport(ep);
        setUpdateInfo(ui);
        applyTheme(p.theme);
      } catch (e) {
        setError(String(e));
      }
    })();
  }, []);

  const savePrefs = useCallback(
    async (next: Preferences) => {
      // Applied immediately, optimistically: the theme is a local visual
      // effect, and waiting for the round trip would leave a click looking
      // like it did nothing for a moment.
      setPrefsState(next);
      applyTheme(next.theme);
      try {
        const saved = await ipc().setPreferences(next);
        setPrefsState(saved);
        applyTheme(saved.theme);
      } catch (e) {
        setError(String(e));
      }
    },
    [],
  );

  const refreshMics = useCallback(async () => {
    try {
      setMics(await listMics());
    } catch {
      setMics([]);
    }
  }, []);

  useEffect(() => {
    void refreshMics();
  }, [refreshMics]);

  const testMic = useCallback(
    async (deviceId: string) => {
      if (micTestingRef.current) return;
      micTestingRef.current = true;
      setMicTesting(true);
      setMicResult("Listening… say something.");
      const rec = createRecorder();
      try {
        await rec.start(undefined, deviceId);
        await new Promise((resolve) => setTimeout(resolve, MIC_TEST_MS));
        const out = await rec.stop();
        setMicResult(describeMicLevel(out.peak) ?? "That sounds good.");
        // Permission is granted now, so the list can show real device names.
        void refreshMics();
      } catch (e) {
        rec.cancel();
        setMicResult(friendlyMicError(e));
      } finally {
        micTestingRef.current = false;
        setMicTesting(false);
      }
    },
    [refreshMics],
  );

  const changeVoice = useCallback(
    async (id: string) => {
      setVoiceState(id);
      try {
        await ipc().setVoice(id);
      } catch (e) {
        setError(String(e));
      }
    },
    [],
  );

  const testVoice = useCallback(async () => {
    if (speaking.current) return;
    speaking.current = true;
    try {
      await speak("This is what the selected voice sounds like.", player.current);
    } catch (e) {
      setError(friendlyTtsError(e));
    } finally {
      speaking.current = false;
    }
  }, []);

  const changeRetention = useCallback(async (next: number) => {
    setRetentionState(next);
    try {
      await ipc().setRetention(next);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const runOptimizer = useCallback(async () => {
    if (optimizing.current) return;
    optimizing.current = true;
    try {
      await ipc().optimizeParameters();
      announce("Scheduling parameters updated from your review history.");
    } catch (e) {
      setError(String(e));
    } finally {
      optimizing.current = false;
    }
  }, [announce]);

  const doExport = useCallback(async () => {
    const path = await ipc().pickSavePath({
      title: "Export all cards",
      defaultName: "olp-export.json",
      extensions: ["json"],
    });
    if (!path) return;
    try {
      const result = await ipc().exportData({ path, format: "json" });
      setNotice(`Exported ${result.cards} card(s) to ${result.path}.`);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const doImport = useCallback(async () => {
    const path = await ipc().pickOpenPath({ title: "Import cards", extensions: ["json", "csv", "tsv"] });
    if (!path) return;
    try {
      const result = await ipc().importData({ path });
      setNotice(
        `Imported ${result.cards_created} new, updated ${result.cards_updated}, skipped ${result.skipped}.`,
      );
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const doBackup = useCallback(async () => {
    const path = await ipc().pickSavePath({
      title: "Backup database",
      defaultName: "olp-backup.sqlite",
      extensions: ["sqlite"],
    });
    if (!path) return;
    try {
      const result = await ipc().backupDatabase(path);
      setNotice(`Backed up to ${result.path} (${result.bytes} bytes).`);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const doSaveDiagnostics = useCallback(async () => {
    const path = await ipc().pickSavePath({
      title: "Save diagnostics",
      defaultName: "olp-diagnostics.txt",
      extensions: ["txt"],
    });
    if (!path) return;
    try {
      await ipc().saveDiagnostics(path);
      setNotice(`Saved diagnostics to ${path}.`);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const doRestore = useCallback(async () => {
    if (restoring.current) return;
    const path = await ipc().pickOpenPath({ title: "Restore database", extensions: ["sqlite", "bak"] });
    if (!path) return;
    restoring.current = true;
    try {
      const info = await ipc().restoreDatabase(path);
      setRestoreInfo(info);
    } catch (e) {
      setError(String(e));
    } finally {
      restoring.current = false;
    }
  }, []);

  const checkForUpdate = useCallback(async () => {
    if (checkingUpdate.current || installing.current) return;
    checkingUpdate.current = true;
    setUpdateStatus({ kind: "checking" });
    try {
      const found = await ipc().checkForUpdate("manual");
      setUpdateStatus(found ? { kind: "available", update: found } : { kind: "newest" });
    } catch (e) {
      setUpdateStatus({ kind: "error", message: `Could not check for updates: ${String(e)}` });
    } finally {
      checkingUpdate.current = false;
    }
  }, []);

  const installUpdate = useCallback(async (version: string) => {
    if (installing.current || checkingUpdate.current) return;
    installing.current = true;
    try {
      await ipc().installUpdate((ev) => {
        switch (ev.kind) {
          case "started":
            setUpdateStatus({ kind: "downloading", version, received: 0, total: ev.total });
            return;
          case "progress":
            setUpdateStatus({ kind: "downloading", version, received: ev.received, total: ev.total });
            return;
          case "verifying":
            setUpdateStatus({ kind: "verifying" });
            return;
          case "installing":
            setUpdateStatus({ kind: "installing" });
            return;
          case "installed":
            setUpdateStatus({ kind: "installed", version });
        }
      });
    } catch (e) {
      setUpdateStatus({ kind: "error", message: `Could not install the update: ${String(e)}` });
    } finally {
      installing.current = false;
    }
  }, []);

  const restartApp = useCallback(async () => {
    try {
      await ipc().restartApp();
    } catch (e) {
      setUpdateStatus({ kind: "error", message: `Could not restart: ${String(e)}` });
    }
  }, []);

  if (!prefs) {
    return (
      <section class="route">
        <h1 tabIndex={-1}>Settings</h1>
        {error ? (
          <p class="notice notice-error" role="alert">
            {error}
          </p>
        ) : (
          <p class="muted">Loading settings…</p>
        )}
      </section>
    );
  }

  return (
    <section class="route route-wide">
      <h1 tabIndex={-1}>Settings</h1>

      {error && (
        <p class="notice notice-error" role="alert">
          {error}
        </p>
      )}
      {notice && <p class="notice">{notice}</p>}

      <section class="settings-section">
        <h2>Models</h2>
        <p class="muted">
          Speech files live outside the app so the installer stays small.
          This is also the way back if you skipped them on first run.
        </p>
        <ModelDownloads announce={announce} />
      </section>

      <section class="settings-section">
        <h2>Voice</h2>
        {voices.length === 0 ? (
          <p class="notice notice-blocking">
            No voice is installed yet, so speech playback is unavailable.
          </p>
        ) : (
          <div class="row" role="radiogroup" aria-label="Voice">
            {voices.map((v) => (
              <label key={v.id}>
                <input
                  type="radio"
                  name="voice"
                  checked={voice === v.id}
                  onChange={() => void changeVoice(v.id)}
                />
                {v.label}
              </label>
            ))}
          </div>
        )}
        <button type="button" disabled={voices.length === 0} onClick={() => void testVoice()}>
          Test voice
        </button>
      </section>

      <section class="settings-section">
        <h2>Practice</h2>
        <div class="row">
          <label for="setting-mic">Microphone</label>
          <select
            id="setting-mic"
            value={prefs.mic_device_id}
            onChange={(e) =>
              void savePrefs({ ...prefs, mic_device_id: (e.target as HTMLSelectElement).value })
            }
          >
            <option value="">System default</option>
            {mics.map((m) => (
              <option key={m.id} value={m.id}>
                {m.label}
              </option>
            ))}
            {prefs.mic_device_id !== "" && !mics.some((m) => m.id === prefs.mic_device_id) && (
              <option value={prefs.mic_device_id}>Saved microphone (not connected)</option>
            )}
          </select>
          <button type="button" disabled={micTesting} onClick={() => void testMic(prefs.mic_device_id)}>
            Test microphone
          </button>
        </div>
        {micResult && (
          <p class="muted" role="status">
            {micResult}
          </p>
        )}
        <div class="row">
          <label for="setting-goal">Practice goal</label>
          <select
            id="setting-goal"
            value={prefs.goal}
            onChange={(e) => void savePrefs({ ...prefs, goal: (e.target as HTMLSelectElement).value })}
          >
            {GOALS.map((g) => (
              <option key={g.id} value={g.id}>
                {g.label}
              </option>
            ))}
          </select>
        </div>
        <div class="row">
          <label for="setting-dialect">Dialect</label>
          <select
            id="setting-dialect"
            value={prefs.dialect}
            onChange={(e) =>
              void savePrefs({ ...prefs, dialect: (e.target as HTMLSelectElement).value })
            }
          >
            {DIALECTS.map((d) => (
              <option key={d} value={d}>
                {d}
              </option>
            ))}
          </select>
        </div>
        <div class="row">
          <label for="setting-practice-goal">Daily speaking goal (attempts, 0 for none)</label>
          <NumberSetting
            id="setting-practice-goal"
            min={0}
            max={100}
            value={prefs.practice_goal_attempts}
            onCommit={(n) => void savePrefs({ ...prefs, practice_goal_attempts: n })}
          />
        </div>
        <div class="row">
          <label for="setting-cutoff">Day cutoff hour</label>
          <NumberSetting
            id="setting-cutoff"
            min={0}
            max={23}
            value={prefs.day_cutoff_hour}
            onCommit={(n) => void savePrefs({ ...prefs, day_cutoff_hour: n })}
          />
        </div>
      </section>

      <section class="settings-section">
        <h2>Scheduling</h2>
        <div class="row">
          <label for="setting-retention">{retentionLabel(retention)}</label>
          <input
            id="setting-retention"
            type="range"
            min={0.7}
            max={0.98}
            step={0.01}
            value={retention}
            // Dragging previews the label; letting go saves. Saving on every
            // step sent a backend call per pixel.
            onInput={(e) => setRetentionState(Number((e.target as HTMLInputElement).value))}
            onChange={(e) => void changeRetention(Number((e.target as HTMLInputElement).value))}
          />
        </div>
        <div class="row">
          <label for="setting-new-per-day">New cards per day</label>
          <NumberSetting
            id="setting-new-per-day"
            min={0}
            value={prefs.new_per_day}
            onCommit={(n) => void savePrefs({ ...prefs, new_per_day: n })}
          />
          <label for="setting-review-per-day">Reviews per day</label>
          <NumberSetting
            id="setting-review-per-day"
            min={0}
            value={prefs.review_per_day}
            onCommit={(n) => void savePrefs({ ...prefs, review_per_day: n })}
          />
        </div>
        <div class="row">
          <button
            type="button"
            disabled={!stats || stats.train_items < stats.min_train_items}
            onClick={() => void runOptimizer()}
          >
            Optimize scheduling parameters
          </button>
          {stats && (
            <span class="muted">
              {stats.train_items} of {stats.min_train_items} reviews needed
              {stats.trainable_cards < stats.min_trainable_cards &&
                ` (and ${stats.min_trainable_cards - stats.trainable_cards} more reviewed cards)`}
              .
            </span>
          )}
        </div>
      </section>

      <section class="settings-section">
        <h2>Data</h2>
        <div class="row">
          <button type="button" onClick={() => void doExport()}>
            Export all cards
          </button>
          <button type="button" onClick={() => void doImport()}>
            Import cards
          </button>
          <button type="button" onClick={() => void doBackup()}>
            Backup database
          </button>
          <button type="button" onClick={() => void doRestore()}>
            Restore database
          </button>
        </div>
        {restoreInfo && (
          <p class="notice notice-blocking">
            Restore staged: {restoreInfo.cards} cards, {restoreInfo.reviews} reviews,{" "}
            {restoreInfo.decks} decks. Restart required to finish restoring.
          </p>
        )}
      </section>

      <section class="settings-section">
        <h2>Updates</h2>
        <p>
          This app only goes online to download the speech models you ask for.
          Checking for updates is off unless you turn it on.
        </p>
        <div class="row">
          <label>
            <input
              type="checkbox"
              checked={prefs.check_updates}
              onChange={(e) =>
                void savePrefs({
                  ...prefs,
                  check_updates: (e.target as HTMLInputElement).checked,
                })
              }
            />
            Check for a new version each time the app starts
          </label>
        </div>
        <p class="muted">
          When this is on, the app asks github.com once per launch whether a
          newer version has been published. The request contains nothing about
          you or your practice. Like any connection, it shows GitHub your IP
          address.
        </p>
        <div class="row">
          <button
            type="button"
            disabled={
              updateStatus.kind === "checking" ||
              updateStatus.kind === "downloading" ||
              updateStatus.kind === "verifying" ||
              updateStatus.kind === "installing"
            }
            onClick={() => void checkForUpdate()}
          >
            Check now
          </button>
        </div>
        <UpdateStatusView
          status={updateStatus}
          info={updateInfo}
          onInstall={(version) => void installUpdate(version)}
          onRestart={() => void restartApp()}
        />
      </section>

      <section class="settings-section">
        <h2>Appearance</h2>
        <div class="row" role="radiogroup" aria-label="Theme">
          {THEMES.map((t) => (
            <label key={t}>
              <input
                type="radio"
                name="theme"
                checked={prefs.theme === t}
                onChange={() => void savePrefs({ ...prefs, theme: t })}
              />
              {t}
            </label>
          ))}
        </div>
      </section>

      <section class="settings-section">
        <h2>Diagnostics</h2>
        {modelStatus && (
          <ul class="diagnostics-list">
            <li>ASR model: {modelStatus.asr_model ? "installed" : "missing"}</li>
            <li>ASR vocabulary: {modelStatus.asr_vocab ? "installed" : "missing"}</li>
            <li>TTS voice: {modelStatus.tts_voice ? "installed" : "missing"}</li>
            {updateInfo && (
              <li>Installed as: {updateInfo.bundle ?? "development build"}</li>
            )}
          </ul>
        )}
        <pre class="output">{epReport}</pre>
        <div class="row">
          <button type="button" onClick={() => void doSaveDiagnostics()}>
            Save diagnostics to a file
          </button>
        </div>
        <p class="muted">
          For a bug report: versions, settings and counts only. No recordings, transcripts, cards or
          microphone name.
        </p>
      </section>
    </section>
  );
}

/** The Updates section's status line and whatever action goes with it. */
function UpdateStatusView(props: {
  status: UpdateStatus;
  info: UpdateInfo | null;
  onInstall: (version: string) => void;
  onRestart: () => void;
}) {
  const { status, info, onInstall, onRestart } = props;
  switch (status.kind) {
    case "idle":
      return null;
    case "checking":
      return <p class="muted">Checking github.com…</p>;
    case "newest":
      return <p>You have the newest version ({info?.current_version ?? "unknown"}).</p>;
    case "available": {
      const { update } = status;
      return (
        <>
          <p>
            Version {update.version} is available. You have {update.current_version}.
          </p>
          {update.can_install ? (
            <>
              <div class="row">
                <button type="button" onClick={() => onInstall(update.version)}>
                  Download and install
                </button>
              </div>
              {(info?.bundle === "msi" || info?.bundle === "nsis") && (
                <p class="muted">
                  The app will close while the installer runs and reopen when it finishes.
                </p>
              )}
            </>
          ) : (
            <>
              {info?.reason && <p>{info.reason}</p>}
              {/* Selectable text, not a link: the webview cannot open a browser. */}
              <p>
                Download it from <code>{info?.release_page}</code>
              </p>
            </>
          )}
        </>
      );
    }
    case "downloading":
      return (
        <p>
          Downloading {status.version}: {formatBytes(status.received)}
          {status.total !== null && ` of ${formatBytes(status.total)}`}
        </p>
      );
    case "verifying":
      return <p>Checking the download's signature…</p>;
    case "installing":
      return <p>Installing…</p>;
    case "installed":
      return (
        <>
          <p>Installed. Restart to use version {status.version}.</p>
          <div class="row">
            <button type="button" onClick={onRestart}>
              Restart now
            </button>
          </div>
        </>
      );
    case "error":
      return <p class="notice notice-error">{status.message}</p>;
  }
}

function applyTheme(theme: string) {
  if (typeof document === "undefined") return;
  if (theme === "light" || theme === "dark") {
    document.documentElement.setAttribute("data-theme", theme);
  } else {
    document.documentElement.removeAttribute("data-theme");
  }
}

/**
 * A whole-number preference saved when the edit is committed (blur, Enter,
 * the spinner), not on every keystroke: typing "25" used to save 2 and then
 * 25, and clearing the field saved 0 because `Number("")` is 0. Anything that
 * is not a whole number goes back to the stored value. Range clamping stays
 * the backend's job; the stored value comes back through `value`.
 */
function NumberSetting(props: {
  id: string;
  min: number;
  max?: number;
  value: number;
  onCommit: (n: number) => void;
}) {
  const { id, min, max, value, onCommit } = props;
  const [draft, setDraft] = useState(String(value));
  useEffect(() => setDraft(String(value)), [value]);
  return (
    <input
      id={id}
      type="number"
      inputMode="numeric"
      min={min}
      max={max}
      value={draft}
      onInput={(e) => setDraft((e.target as HTMLInputElement).value)}
      onChange={(e) => {
        const el = e.target as HTMLInputElement;
        const raw = el.value.trim();
        if (!/^\d+$/.test(raw)) {
          // Also on the element: if the typing and this revert render
          // together, the draft never visibly changed and Preact leaves
          // the stale text in the box.
          el.value = String(value);
          setDraft(String(value));
          return;
        }
        const n = Number(raw);
        if (n === value) return;
        onCommit(n);
      }}
    />
  );
}
