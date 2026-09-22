import { useCallback, useEffect, useRef, useState } from "preact/hooks";

import { ipc } from "../ipc/commands";
import { createRecorder } from "../app/recorder";
import { ModelDownloads } from "../components/ModelDownloads";
import type { Preferences } from "../ipc/types";

/**
 * First run.
 *
 * This screen exists because the installer deliberately does not carry the
 * weights: a ~440 MB download that most users only need once should not be
 * in every installer, and shipping it would also mean re-shipping it on
 * every patch release. The cost of that choice is that a fresh install
 * cannot hear or speak until something fetches the models — and the thing
 * that does has to be this, not a README telling a GUI user to run a shell
 * script.
 */

const GOALS = [
  {
    id: "everyday",
    label: "Everyday conversation",
    detail: "Small talk, phone calls, ordering, disagreeing politely.",
  },
  {
    id: "interview",
    label: "Job interviews",
    detail: "Tell me about yourself, behavioural answers, salary questions.",
  },
  {
    id: "both",
    label: "Both",
    detail: "Mix prompts from everyday life and interviews.",
  },
] as const;

const STEP_TITLES = [
  "Welcome",
  "What do you want to practise?",
  "Set up your voice",
  "Check your microphone",
] as const;

export function Onboarding(props: {
  announce: (msg: string) => void;
  onFinish: () => void;
}) {
  const { announce, onFinish } = props;
  const [step, setStep] = useState(0);
  const [goal, setGoal] = useState<string>("both");
  const [missing, setMissing] = useState(1);
  const [error, setError] = useState<string | null>(null);

  const [micState, setMicState] = useState<"idle" | "listening" | "ok" | "quiet" | "denied">(
    "idle",
  );
  const [level, setLevel] = useState(0);
  const recorder = useRef(createRecorder());
  const micPeak = useRef(0);

  useEffect(() => {
    void ipc()
      .getPreferences()
      .then((p) => setGoal(p.goal || "both"))
      .catch((e) => setError(String(e)));
    // Whatever happens, never leave the microphone open behind us.
    const rec = recorder.current;
    return () => rec.cancel();
  }, []);

  const runMicCheck = useCallback(async () => {
    setMicState("listening");
    micPeak.current = 0;
    try {
      await recorder.current.start((peak) => {
        micPeak.current = Math.max(micPeak.current, peak);
        setLevel(peak);
      });
    } catch {
      setMicState("denied");
      return;
    }
    // Long enough to say a few words, short enough not to feel like a form.
    setTimeout(() => {
      void (async () => {
        try {
          await recorder.current.stop();
        } catch {
          // The captured peak is what matters; a failed resample at the
          // end of a mic test is not worth surfacing.
        }
        setLevel(0);
        // 0.02 is roughly the noise floor of a quiet room: below it, the
        // mic is almost certainly muted or feeding the wrong device.
        setMicState(micPeak.current > 0.02 ? "ok" : "quiet");
      })();
    }, 4000);
  }, []);

  const finish = useCallback(async () => {
    try {
      const prefs = await ipc().getPreferences();
      const next: Preferences = { ...prefs, goal, onboarded: true };
      await ipc().setPreferences(next);
    } catch (e) {
      // Persisting the flag is best-effort: failing to save it must not
      // trap someone on the last screen of onboarding forever.
      setError(String(e));
    }
    recorder.current.cancel();
    onFinish();
  }, [goal, onFinish]);

  return (
    <section class="route" aria-labelledby="onboarding-title">
      <div class="route-head">
        <h1 id="onboarding-title" tabIndex={-1}>
          {STEP_TITLES[step]}
        </h1>
        <p class="muted">Step {step + 1} of {STEP_TITLES.length}</p>
      </div>

      {error && (
        <p class="notice notice-error" role="alert">
          {error}
        </p>
      )}

      {step === 0 && (
        <div class="settings-section">
          <p>
            Practise speaking English out loud and get told what to fix. It
            runs entirely on this computer: nothing you say is uploaded, and
            it works with the network off.
          </p>
          <p class="muted">
            One thing to set up first — the speech models are downloaded
            separately, so the installer stays small.
          </p>
          <div class="row">
            <button type="button" onClick={() => setStep(1)}>
              Get started
            </button>
          </div>
        </div>
      )}

      {step === 1 && (
        <div class="settings-section">
          <div class="deck-list" role="radiogroup" aria-label="Practice goal">
            {GOALS.map((g) => (
              <button
                key={g.id}
                type="button"
                role="radio"
                aria-checked={goal === g.id}
                class={goal === g.id ? "deck-item deck-item-active" : "deck-item"}
                onClick={() => setGoal(g.id)}
              >
                <strong>{g.label}</strong>
                <span class="muted">{g.detail}</span>
              </button>
            ))}
          </div>
          <div class="row">
            <button type="button" class="muted" onClick={() => setStep(0)}>
              Back
            </button>
            <button type="button" onClick={() => setStep(2)}>
              Continue
            </button>
          </div>
        </div>
      )}

      {step === 2 && (
        <div class="settings-section">
          <ModelDownloads announce={announce} onMissingChange={setMissing} />
          <div class="row">
            <button type="button" onClick={() => setStep(3)}>
              {missing === 0 ? "Continue" : "Skip for now"}
            </button>
          </div>
          {missing > 0 && (
            <p class="muted">
              You can skip this, but speaking practice needs the speech
              recognition files and Listen needs the voice. You can come back
              to it any time from Settings.
            </p>
          )}
        </div>
      )}

      {step === 3 && (
        <div class="settings-section">
          <p>
            Say something for a few seconds — anything at all. This only
            checks that the app can hear you; nothing is saved.
          </p>
          {micState === "listening" && (
            <div class="meter">
              <span class="meter-label">Listening</span>
              <progress value={Math.min(1, level * 4)} max={1} />
            </div>
          )}
          {micState === "ok" && (
            <p class="notice">Heard you clearly. You are all set.</p>
          )}
          {micState === "quiet" && (
            <p class="notice notice-error">
              Nothing came through. Check that the right microphone is
              selected and not muted, then try again.
            </p>
          )}
          {micState === "denied" && (
            <p class="notice notice-error">
              The microphone was blocked. Allow access in your system
              settings, then try again — you can also do this later.
            </p>
          )}
          <div class="row">
            {micState !== "listening" && (
              <button type="button" onClick={() => void runMicCheck()}>
                {micState === "idle" ? "Start test" : "Try again"}
              </button>
            )}
            <button type="button" onClick={() => void finish()}>
              {micState === "ok" ? "Start practising" : "Finish"}
            </button>
          </div>
        </div>
      )}
    </section>
  );
}
