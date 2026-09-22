import { useCallback, useEffect, useRef, useState } from "preact/hooks";

import { ipc } from "../ipc/commands";
import type { AttemptReport, LintReport, PromptView } from "../ipc/types";
import { createRecorder, MAX_RECORDING_MS } from "../app/recorder";
import { friendlyAsrError } from "../lib/errors";
import { goPrefix } from "../lib/globalKeys";
import { practiceKeyAction } from "../lib/keyboard";
import {
  DeliveryNote,
  LintedText,
  Meter,
  WordAlignmentView,
  WordScoreView,
} from "../components/Meter";

type Stage = "prompt" | "recording" | "scoring" | "feedback";

const CATEGORIES = [
  { id: "conversation", label: "Everyday conversation" },
  { id: "interview", label: "Job interview" },
] as const;

export function Practice(props: { announce: (msg: string) => void }) {
  const { announce } = props;
  const [category, setCategory] = useState<string>("conversation");
  const [sessionId, setSessionId] = useState<string | undefined>(undefined);
  const [prompt, setPrompt] = useState<PromptView | null>(null);
  const [stage, setStage] = useState<Stage>("prompt");
  const [report, setReport] = useState<AttemptReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [elapsedMs, setElapsedMs] = useState(0);
  const [level, setLevel] = useState(0);
  const [asrReady, setAsrReady] = useState(true);
  const recorder = useRef(createRecorder());
  const timer = useRef<ReturnType<typeof setInterval> | undefined>(undefined);

  // The Lab's one irreplaceable feature: checking arbitrary writing, not
  // just a transcript. Kept small and out of the way in a <details> panel so
  // it does not compete with the practice flow above it.
  const [checkText, setCheckText] = useState("");
  const [checkLint, setCheckLint] = useState<LintReport | null>(null);
  const [checking, setChecking] = useState(false);
  const checkingRef = useRef(false);

  const runCheck = useCallback(async () => {
    if (checkingRef.current || checkText.trim() === "") return;
    checkingRef.current = true;
    setChecking(true);
    try {
      const lint = await ipc().lintText(checkText);
      setCheckLint(lint);
    } catch (e) {
      setError(String(e));
    } finally {
      checkingRef.current = false;
      setChecking(false);
    }
  }, [checkText]);

  const loadPrompt = useCallback(
    async (session: string | undefined, cat: string) => {
      setError(null);
      setReport(null);
      setStage("prompt");
      try {
        const next = await ipc().nextPrompt({
          ...(session === undefined ? {} : { sessionId: session }),
          category: cat,
        });
        setPrompt(next);
        if (next) announce(next.prompt_text);
      } catch (e) {
        setError(String(e));
      }
    },
    [announce],
  );

  // Open a session per category, so "skip what I already did" is scoped to
  // the thing actually being practised.
  useEffect(() => {
    let live = true;
    void (async () => {
      try {
        const status = await ipc().modelStatus();
        if (live) setAsrReady(status.asr_model && status.asr_vocab);
      } catch {
        // Status unknown: assume usable rather than locking out a working
        // install because one probe failed.
        if (live) setAsrReady(true);
      }
      try {
        const id = await ipc().startSession(category);
        if (!live) return;
        setSessionId(id);
        await loadPrompt(id, category);
      } catch (e) {
        if (live) setError(String(e));
      }
    })();
    return () => {
      live = false;
    };
  }, [category, loadPrompt]);

  // Never leave the microphone open when the route unmounts.
  useEffect(() => {
    const active = recorder.current;
    return () => {
      active.cancel();
      clearInterval(timer.current);
    };
  }, []);

  const stopAndScore = useCallback(async () => {
    clearInterval(timer.current);
    if (!recorder.current.isRecording()) return;
    setStage("scoring");
    try {
      const rec = await recorder.current.stop();
      const report = await ipc().scoreAttempt({
        pcm: rec.pcm,
        sampleRate: 16000,
        ...(sessionId === undefined ? {} : { sessionId }),
        ...(prompt ? { promptId: prompt.id } : {}),
        ...(prompt?.target_text ? { targetText: prompt.target_text } : {}),
      });
      setReport(report);
      setStage("feedback");
      announce(
        report.pron_overall === null
          ? `Scored. Grammar ${report.grammar_score} out of 100.`
          : `Scored. ${report.pron_overall} out of 100 on the words.`,
      );
    } catch (e) {
      setError(friendlyAsrError(e));
      setStage("prompt");
    }
  }, [announce, prompt, sessionId]);

  const startRecording = useCallback(async () => {
    setError(null);
    setElapsedMs(0);
    try {
      await recorder.current.start(setLevel);
      setStage("recording");
      announce("Recording.");
      const startedAt = Date.now();
      timer.current = setInterval(() => {
        const ms = Date.now() - startedAt;
        setElapsedMs(ms);
        // Hard stop at the backend's cap rather than letting someone talk
        // past it and get the whole recording rejected.
        if (ms >= MAX_RECORDING_MS) void stopAndScore();
      }, 100);
    } catch (e) {
      setError(`Microphone unavailable: ${String(e)}`);
    }
  }, [announce, stopAndScore]);

  const speakPrompt = useCallback(async () => {
    const text = prompt?.target_text ?? prompt?.prompt_text;
    if (!text) return;
    try {
      const chunks: BlobPart[] = [];
      await ipc().synthesizeSpeech(text, (buf) => chunks.push(new Uint8Array(buf)));
      const audio = new Audio(URL.createObjectURL(new Blob(chunks, { type: "audio/wav" })));
      void audio.play();
    } catch (e) {
      setError(`Could not play the prompt: ${String(e)}`);
    }
  }, [prompt]);

  // Same pattern as the review keys: the rule is pure and tested, and the
  // listener reads current state through a ref so it never has to be
  // re-registered — a listener rebuilt on every render is stale for exactly
  // as long as it takes effects to flush, which is long enough to drop a
  // keystroke.
  const latest = useRef({ stage, prompt, asrReady, startRecording, stopAndScore, speakPrompt });
  latest.current = { stage, prompt, asrReady, startRecording, stopAndScore, speakPrompt };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const now = latest.current;
      const target = e.target as HTMLElement | null;
      const action = practiceKeyAction(e.key, {
        targetTag: target?.tagName ?? "",
        isContentEditable: target?.isContentEditable ?? false,
        isComposing: e.isComposing,
        goPending: goPrefix.armed,
        hasPrompt: now.prompt !== null,
        recording: now.stage === "recording",
        canRecord: now.asrReady && now.stage !== "scoring",
      });
      if (!action) return;
      e.preventDefault();
      if (action.kind === "record") void now.startRecording();
      else if (action.kind === "stop") void now.stopAndScore();
      else void now.speakPrompt();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);

  const saveToReview = useCallback(async () => {
    if (!prompt?.target_text) return;
    try {
      await ipc().addCard("default", prompt.target_text, prompt.prompt_text);
      announce("Saved to your review deck.");
    } catch (e) {
      setError(String(e));
    }
  }, [announce, prompt]);

  return (
    <section class="route">
      <header class="route-head">
        <h1 tabIndex={-1}>Practice</h1>
        <div class="segmented" role="group" aria-label="What to practise">
          {CATEGORIES.map((c) => (
            <button
              key={c.id}
              type="button"
              aria-pressed={category === c.id}
              onClick={() => setCategory(c.id)}
            >
              {c.label}
            </button>
          ))}
        </div>
      </header>

      {!asrReady && (
        <p class="notice notice-blocking">
          The speech model is not installed yet, so recordings cannot be scored.
          Install it and reopen this screen.
        </p>
      )}
      {error && <p class="notice notice-error">{error}</p>}

      {!prompt && !error && <p class="muted">Loading a prompt…</p>}

      {prompt && (
        <article class="prompt-card">
          <div class="prompt-meta">
            <span class="chip">{prompt.topic}</span>
            <span class="chip">Level {prompt.level}</span>
            <span class="chip">
              {prompt.target_text ? "Read aloud" : "Free speaking"}
            </span>
          </div>
          <p class="prompt-framing">{prompt.prompt_text}</p>
          {prompt.target_text && <p class="prompt-target">{prompt.target_text}</p>}
          <div class="row">
            <button type="button" onClick={() => void speakPrompt()}>
              Listen <kbd>P</kbd>
            </button>
            <button
              type="button"
              onClick={() => void loadPrompt(sessionId, category)}
              disabled={stage === "recording" || stage === "scoring"}
            >
              Skip
            </button>
          </div>
        </article>
      )}

      <div class="row record-row">
        {stage !== "recording" ? (
          <button
            type="button"
            class="primary"
            disabled={!asrReady || !prompt || stage === "scoring"}
            onClick={() => void startRecording()}
          >
            {stage === "feedback" ? "Try again" : "Record"} <kbd>R</kbd>
          </button>
        ) : (
          <button type="button" class="primary recording" onClick={() => void stopAndScore()}>
            Stop <kbd>R</kbd>
          </button>
        )}
        {stage === "recording" && (
          <>
            <span class="timer">{(elapsedMs / 1000).toFixed(1)}s</span>
            <span class="level" aria-hidden="true">
              <span style={{ width: `${Math.min(100, level * 300)}%` }} />
            </span>
          </>
        )}
        {stage === "scoring" && <span class="muted">Listening back…</span>}
      </div>

      {report && stage === "feedback" && (
        <article class="feedback">
          <div class="meters">
            <Meter
              label="Pronunciation"
              value={report.pron_overall}
              unmeasuredNote="Not scored for free speaking"
            />
            <Meter label="Grammar" value={report.grammar_score} />
            <Meter
              label="Fluency"
              value={report.fluency?.score ?? null}
              unmeasuredNote="Say a few more words to measure this"
            />
          </div>

          {report.fluency && <DeliveryNote fluency={report.fluency} />}

          <h2>What you said</h2>
          <LintedText text={report.transcript} diags={report.lint.diags} />
          {report.lint.truncated && (
            <p class="muted">Only the first part was checked for grammar.</p>
          )}

          {report.pron && (
            <>
              <h2>How clearly you said it</h2>
              <WordScoreView words={report.pron.words} />
            </>
          )}

          {report.alignment && (
            <>
              <h2>Against the target</h2>
              <WordAlignmentView ops={report.alignment.ops} />
              <p class="muted">
                {report.alignment.matched} of{" "}
                {report.alignment.matched + report.alignment.substituted + report.alignment.deleted}{" "}
                words matched
                {report.alignment.inserted > 0 && `, ${report.alignment.inserted} extra`}.
              </p>
            </>
          )}

          <p class="muted">
            Overall {report.overall} — based on {report.overall_basis.join(" and ")}.
          </p>
          {report.pron_method === "text" && (
            <p class="muted">
              Pronunciation here is word-by-word matching: the close listen
              could not run on this recording.
            </p>
          )}

          <div class="row">
            <button type="button" class="primary" onClick={() => void loadPrompt(sessionId, category)}>
              Next prompt
            </button>
            {prompt?.target_text && (
              <button type="button" onClick={() => void saveToReview()}>
                Save phrase to review
              </button>
            )}
          </div>
        </article>
      )}

      <details class="check-writing">
        <summary>Check writing</summary>
        <div class="row">
          <textarea
            value={checkText}
            onInput={(e) => setCheckText((e.target as HTMLTextAreaElement).value)}
            placeholder="Paste or type anything to check its grammar…"
            rows={4}
          />
        </div>
        <div class="row">
          <button
            type="button"
            disabled={checking || checkText.trim() === ""}
            onClick={() => void runCheck()}
          >
            Check
          </button>
        </div>
        {checkLint && (
          <>
            <LintedText text={checkText} diags={checkLint.diags} />
            {checkLint.truncated && (
              <p class="muted">Only the first part was checked.</p>
            )}
            {checkLint.diags.length === 0 && <p class="muted">No issues found.</p>}
          </>
        )}
      </details>
    </section>
  );
}
