import { useCallback, useEffect, useRef, useState } from "preact/hooks";

import { ipc } from "../ipc/commands";
import { ModelDownloads } from "../components/ModelDownloads";
import type {
  BackupInfo,
  ModelStatus,
  Preferences,
  ReviewStats,
  VoiceInfo,
} from "../ipc/types";

const DIALECTS = ["american", "british", "canadian", "australian"] as const;
const THEMES = ["system", "light", "dark"] as const;

/** "Remember about N out of 100" reads better than a bare decimal retention. */
function retentionLabel(r: number): string {
  return `Remember about ${Math.round(r * 100)} out of 100`;
}

export function Settings(props: { announce: (msg: string) => void }) {
  const { announce } = props;
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

  const optimizing = useRef(false);
  const restoring = useRef(false);

  useEffect(() => {
    void (async () => {
      try {
        const [v, id, p, r, s, ms, ep] = await Promise.all([
          ipc().listVoices(),
          ipc().getVoice(),
          ipc().getPreferences(),
          ipc().getRetention(),
          ipc().reviewStats(),
          ipc().modelStatus(),
          ipc().epReport(),
        ]);
        setVoices(v);
        setVoiceState(id);
        setPrefsState(p);
        setRetentionState(r);
        setStats(s);
        setModelStatus(ms);
        setEpReport(ep);
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
    try {
      const chunks: BlobPart[] = [];
      await ipc().synthesizeSpeech("This is what the selected voice sounds like.", (buf) =>
        chunks.push(new Uint8Array(buf)),
      );
      const audio = new Audio(URL.createObjectURL(new Blob(chunks, { type: "audio/wav" })));
      void audio.play();
    } catch (e) {
      setError(String(e));
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
          <label for="setting-cutoff">Day cutoff hour</label>
          <input
            id="setting-cutoff"
            type="number"
            min={0}
            max={23}
            value={prefs.day_cutoff_hour}
            onInput={(e) =>
              void savePrefs({
                ...prefs,
                day_cutoff_hour: Number((e.target as HTMLInputElement).value),
              })
            }
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
            onInput={(e) => void changeRetention(Number((e.target as HTMLInputElement).value))}
          />
        </div>
        <div class="row">
          <label for="setting-new-per-day">New cards per day</label>
          <input
            id="setting-new-per-day"
            type="number"
            min={0}
            value={prefs.new_per_day}
            onInput={(e) =>
              void savePrefs({ ...prefs, new_per_day: Number((e.target as HTMLInputElement).value) })
            }
          />
          <label for="setting-review-per-day">Reviews per day</label>
          <input
            id="setting-review-per-day"
            type="number"
            min={0}
            value={prefs.review_per_day}
            onInput={(e) =>
              void savePrefs({
                ...prefs,
                review_per_day: Number((e.target as HTMLInputElement).value),
              })
            }
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
          </ul>
        )}
        <pre class="output">{epReport}</pre>
      </section>
    </section>
  );
}

function applyTheme(theme: string) {
  if (typeof document === "undefined") return;
  if (theme === "light" || theme === "dark") {
    document.documentElement.setAttribute("data-theme", theme);
  } else {
    document.documentElement.removeAttribute("data-theme");
  }
}
