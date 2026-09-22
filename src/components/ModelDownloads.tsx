import { useCallback, useEffect, useState } from "preact/hooks";

import { ipc } from "../ipc/commands";
import type { DownloadEvent, ModelGroup } from "../ipc/types";

/**
 * The model download panel, shared by first-run onboarding and Settings.
 *
 * It lives in both places on purpose. Onboarding can be skipped — a metered
 * connection is a good reason to defer half a gigabyte — and a skip that
 * cannot be undone would leave the app permanently unable to hear anyone.
 * Settings is the way back.
 */

/** Sizes are shown so someone on a metered connection can decide. */
export function formatBytes(n: number): string {
  if (n < 1000) return `${n} B`;
  if (n < 1000 * 1000) return `${Math.round(n / 1000)} KB`;
  return `${(n / 1000 / 1000).toFixed(n < 100 * 1000 * 1000 ? 1 : 0)} MB`;
}

type Phase = "idle" | "running" | "paused" | "done" | "error";

export function ModelDownloads(props: {
  announce: (msg: string) => void;
  /** Called with the number of groups still missing, whenever it changes. */
  onMissingChange?: (missing: number) => void;
}) {
  const { announce, onMissingChange } = props;
  const [groups, setGroups] = useState<ModelGroup[]>([]);
  const [dir, setDir] = useState("");
  const [phase, setPhase] = useState<Phase>("idle");
  const [active, setActive] = useState<string | null>(null);
  const [received, setReceived] = useState(0);
  const [total, setTotal] = useState(0);
  const [verifying, setVerifying] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const publish = useCallback(
    (next: ModelGroup[]) => {
      setGroups(next);
      onMissingChange?.(next.filter((g) => !g.installed).length);
    },
    [onMissingChange],
  );

  useEffect(() => {
    void (async () => {
      try {
        const [cat, d] = await Promise.all([ipc().listModelCatalog(), ipc().modelsDir()]);
        publish(cat);
        setDir(d);
      } catch (e) {
        setError(String(e));
      }
    })();
  }, [publish]);

  const refresh = useCallback(async () => {
    try {
      publish(await ipc().listModelCatalog());
    } catch {
      // A failed refresh only leaves the badges stale; whatever was
      // downloaded is on disk either way.
    }
  }, [publish]);

  const onEvent = useCallback((ev: DownloadEvent) => {
    switch (ev.kind) {
      case "started":
        setActive(ev.id);
        setReceived(0);
        setTotal(ev.total);
        setVerifying(false);
        return;
      case "progress":
        setReceived(ev.received);
        setTotal(ev.total);
        return;
      case "verifying":
        setVerifying(true);
        return;
      case "installed":
        setVerifying(false);
        return;
      case "done":
        setActive(null);
        setVerifying(false);
        return;
      case "failed":
        setError(ev.message);
        return;
      case "cancelled":
        setActive(null);
        return;
    }
  }, []);

  const start = useCallback(async () => {
    setError(null);
    setPhase("running");
    try {
      await ipc().downloadModels([], onEvent);
      setPhase("done");
      announce("Models installed.");
    } catch (e) {
      // A cancel rejects through the same path as a real failure; calling
      // a deliberate cancel an error would be a lie.
      const msg = String(e);
      if (msg.toLowerCase().includes("cancel")) {
        setPhase("idle");
        announce("Download cancelled.");
      } else {
        setPhase("error");
        setError(msg);
      }
    } finally {
      setActive(null);
      setVerifying(false);
      await refresh();
    }
  }, [announce, onEvent, refresh]);

  const pause = useCallback(async () => {
    await ipc().pauseDownloads();
    setPhase("paused");
    announce("Download paused.");
  }, [announce]);

  const resume = useCallback(async () => {
    await ipc().resumeDownloads();
    setPhase("running");
    announce("Download resumed.");
  }, [announce]);

  const busy = phase === "running" || phase === "paused";
  const pct = total > 0 ? Math.min(100, Math.round((received / total) * 100)) : 0;
  const missing = groups.filter((g) => !g.installed);

  return (
    <div>
      {error && (
        <p class="notice notice-error" role="alert">
          {error}
        </p>
      )}

      <table class="cards-table">
        <caption class="visually-hidden">Model downloads</caption>
        <thead>
          <tr>
            <th scope="col">What it does</th>
            <th scope="col">Size</th>
            <th scope="col">Status</th>
          </tr>
        </thead>
        <tbody>
          {groups.map((g) => (
            <tr key={g.id}>
              <td>
                <strong>{g.label}</strong>
                <br />
                <span class="muted">{g.detail}</span>
              </td>
              <td>{formatBytes(g.bytes)}</td>
              <td>
                {g.installed ? (
                  <span class="chip">Installed</span>
                ) : active === g.id ? (
                  <span>{verifying ? "Checking…" : `${pct}%`}</span>
                ) : (
                  <span class="muted">Not installed</span>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>

      {busy && (
        <div class="meter">
          <span class="meter-label">
            {verifying ? "Checking the file is intact…" : "Downloading…"}
          </span>
          <progress value={received} max={total || 1} />
          <span class="meter-value">
            {formatBytes(received)} of {formatBytes(total)}
          </span>
        </div>
      )}

      <div class="row">
        {!busy && missing.length > 0 && (
          <button type="button" onClick={() => void start()}>
            Download {formatBytes(missing.reduce((n, g) => n + g.bytes, 0))}
          </button>
        )}
        {phase === "running" && (
          <button type="button" onClick={() => void pause()}>
            Pause
          </button>
        )}
        {phase === "paused" && (
          <button type="button" onClick={() => void resume()}>
            Resume
          </button>
        )}
        {busy && (
          <button type="button" onClick={() => void ipc().cancelDownloads()}>
            Cancel
          </button>
        )}
      </div>

      {!busy && missing.length > 0 && dir && (
        <p class="muted">
          Already have these files? Put them in <code>{dir}</code>.
        </p>
      )}
    </div>
  );
}
