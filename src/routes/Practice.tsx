import { useCallback, useEffect, useRef, useState } from "preact/hooks";

import { ipc } from "../ipc/commands";
import type { AttemptReport, PromptView } from "../ipc/types";
import { createRecorder, MAX_RECORDING_MS } from "../app/recorder";
import { friendlyAsrError } from "../lib/errors";
import { LintedText, Meter, WordAlignmentView } from "../components/Meter";

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
              Listen
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
            {stage === "feedback" ? "Try again" : "Record"}
          </button>
        ) : (
          <button type="button" class="primary recording" onClick={() => void stopAndScore()}>
            Stop
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
              value={null}
              unmeasuredNote="Coming soon"
            />
          </div>

          <h2>What you said</h2>
          <LintedText text={report.transcript} diags={report.lint.diags} />
          {report.lint.truncated && (
            <p class="muted">Only the first part was checked for grammar.</p>
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
    </section>
  );
}
