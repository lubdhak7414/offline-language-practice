import { useCallback, useEffect, useRef, useState } from "preact/hooks";

import { events } from "../ipc/events";
import { ipc } from "../ipc/commands";
import { globalKeyAction, goPrefix } from "../lib/globalKeys";
import { navigate, route, startRouter, type Route } from "./router";
import { KeyboardHelp } from "../components/KeyboardHelp";
import { Practice } from "../routes/Practice";
import { Review } from "../routes/Review";
import { Decks } from "../routes/Decks";
import { Progress } from "../routes/Progress";
import { Settings } from "../routes/Settings";
import { Onboarding } from "../routes/Onboarding";

const NAV: Array<{ id: Route; label: string; ready: boolean }> = [
  { id: "practice", label: "Practice", ready: true },
  { id: "review", label: "Review", ready: true },
  { id: "decks", label: "Decks", ready: true },
  { id: "progress", label: "Progress", ready: true },
  { id: "settings", label: "Settings", ready: true },
];

/** How long a transient announcement stays on screen. */
const TOAST_MS = 4000;

export function App() {
  const [toast, setToast] = useState("");
  /**
   * `null` while we do not yet know. Rendering the main shell during that
   * gap would flash Practice at a first-run user before replacing it with
   * onboarding, and rendering onboarding would do the same to everyone
   * else — so the shell renders nothing until preferences answer.
   */
  const [onboarded, setOnboarded] = useState<boolean | null>(null);
  const [helpOpen, setHelpOpen] = useState(false);
  const heading = useRef<HTMLDivElement>(null);
  const toastTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

  /**
   * The app's single announcement channel.
   *
   * One live region, not one per panel: a screen reader given seven
   * competing regions reads them in an order nobody controls, so most of
   * what it says is noise.
   */
  const announce = useCallback((msg: string) => {
    setToast(msg);
    clearTimeout(toastTimer.current);
    toastTimer.current = setTimeout(() => setToast(""), TOAST_MS);
  }, []);

  useEffect(() => startRouter(), []);

  useEffect(() => {
    void ipc()
      .getPreferences()
      .then((p) => setOnboarded(p.onboarded))
      .catch(() => {
        // No backend (tests, browser preview) or an unreadable database:
        // show the app rather than trapping someone in onboarding.
        setOnboarded(true);
      });
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    void events()
      .on("system-status", (payload) => {
        if (payload) announce(payload);
      })
      .then((fn) => {
        unlisten = fn;
      })
      .catch(() => {
        // No Tauri host (tests, browser preview): events are simply absent.
      });
    return () => unlisten?.();
  }, [announce]);

  // Focus the new route's heading so keyboard and screen-reader users are
  // not left at the top of the nav after every navigation.
  const current = route.value;
  useEffect(() => {
    heading.current?.querySelector<HTMLElement>("h1")?.focus();
  }, [current]);

  // The global map. Route-level shortcuts (grades, record) are handled in
  // their own routes; anything this rule does not claim passes straight
  // through to them.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      const action = globalKeyAction(e.key, {
        targetTag: target?.tagName ?? "",
        isContentEditable: target?.isContentEditable ?? false,
        isComposing: e.isComposing,
        hasModifier: e.metaKey || e.ctrlKey || e.altKey,
        helpOpen,
        pendingGo: goPrefix.armed,
      });
      if (!action) return;
      e.preventDefault();
      switch (action.kind) {
        case "go":
          goPrefix.armed = true;
          return;
        case "cancel":
          goPrefix.armed = false;
          return;
        case "navigate":
          goPrefix.armed = false;
          navigate(action.route);
          return;
        case "help":
          goPrefix.armed = false;
          setHelpOpen(action.open);
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [helpOpen]);

  if (onboarded === null) return <div class="shell" />;

  if (!onboarded) {
    return (
      <div class="shell">
        <main class="content" ref={heading}>
          <Onboarding announce={announce} onFinish={() => setOnboarded(true)} />
        </main>
        <div class="toast" role="status" aria-live="polite">
          {toast}
        </div>
      </div>
    );
  }

  return (
    <div class="shell">
      <nav class="rail" aria-label="Main">
        <span class="wordmark">Practice</span>
        {NAV.map((item) => (
          <button
            key={item.id}
            type="button"
            class="rail-link"
            aria-current={current === item.id ? "page" : undefined}
            onClick={() => navigate(item.id)}
          >
            {item.label}
            {!item.ready && <span class="rail-soon">soon</span>}
          </button>
        ))}
      </nav>

      <main class="content" ref={heading}>
        {current === "practice" && <Practice announce={announce} />}
        {current === "review" && <Review announce={announce} />}
        {current === "decks" && <Decks announce={announce} />}
        {current === "progress" && <Progress />}
        {current === "settings" && <Settings announce={announce} />}
      </main>

      {/*
        The one toast region for the whole app. `role="status"` is polite, so
        it never interrupts what a screen reader is already reading.
      */}
      <div class="toast" role="status" aria-live="polite">
        {toast}
      </div>

      {helpOpen && <KeyboardHelp onClose={() => setHelpOpen(false)} />}
    </div>
  );
}
