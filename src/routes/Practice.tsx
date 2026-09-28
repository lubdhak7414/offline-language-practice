import { useCallback, useEffect, useRef, useState } from "preact/hooks";

import { ipc } from "../ipc/commands";
import type { AttemptReport, LintReport, PromptView, SessionSummary } from "../ipc/types";
import { createRecorder, MAX_RECORDING_MS } from "../app/recorder";
import { createClipPlayer, speak } from "../lib/audio/player";
import { f32ToWav } from "../lib/audio/wav";
import { friendlyAsrError, friendlyMicError, friendlyTtsError } from "../lib/errors";
import { categoryForGoal } from "../lib/goals";
import { OwnPrompts } from "../components/OwnPrompts";
import { describeMicLevel } from "../lib/micLevel";
import { answerLengthNote, targetSeconds } from "../lib/answerLength";
import { describeChange } from "../lib/attemptDelta";
import { describeDailyGoal } from "../lib/practiceGoal";
import { tzOffsetMinutes } from "../lib/tz";
import { goPrefix } from "../lib/globalKeys";
import { practiceKeyAction } from "../lib/keyboard";
import { describeSession } from "../lib/sessionSummary";
import {
  DeliveryNote,
  LintedText,
  FlagNote,
  Meter,
  WordAlignmentView,
  WordScoreView,
} from "../components/Meter";

type Stage = "prompt" | "recording" | "scoring" | "feedback";

const CATEGORIES = [
  { id: "conversation", label: "Everyday conversation" },
  { id: "interview", label: "Job interview" },
] as const;

/** `null` is every level; the built-in prompts use 1–3. */
const LEVELS: ReadonlyArray<{ id: number | null; label: string }> = [
  { id: null, label: "Any level" },
  { id: 1, label: "Level 1" },
  { id: 2, label: "Level 2" },
  { id: 3, label: "Level 3" },
];

export function Practice(props: { announce: (msg: string) => void }) {
  const { announce } = props;
  const [category, setCategory] = useState<string>("conversation");
  // The session starts only once the saved goal has picked the category, so
  // the first prompt is never the wrong kind.
  const [goalLoaded, setGoalLoaded] = useState(false);
  const [dailyGoal, setDailyGoal] = useState(0);
  // The chosen microphone, read when a recording starts.
  const micRef = useRef("");
  const [micNote, setMicNote] = useState<string | null>(null);
  const [attemptsToday, setAttemptsToday] = useState(0);
  const [sessionId, setSessionId] = useState<string | undefined>(undefined);
  const [summary, setSummary] = useState<SessionSummary | null>(null);
  // The session a late summary reply belongs to; a reply for an older one is dropped.
  const currentSession = useRef<string | undefined>(undefined);
  const [prompt, setPrompt] = useState<PromptView | null>(null);
  const [stage, setStage] = useState<Stage>("prompt");
  const [report, setReport] = useState<AttemptReport | null>(null);
  // The report before the one on screen, for "up 8 points from your last try".
  // Only kept while the prompt stays the same; a new prompt starts clean.
  const reportRef = useRef<AttemptReport | null>(null);
  reportRef.current = report;
  const [change, setChange] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [elapsedMs, setElapsedMs] = useState(0);
  const [level, setLevel] = useState(0);
  const [asrReady, setAsrReady] = useState(true);
  const recorder = useRef(createRecorder());
  const player = useRef(createClipPlayer());
  // One Listen at a time: the TTS worker queues, so a double-press would
  // otherwise play the prompt twice back to back.
  const speakingRef = useRef(false);
  // The last attempt's audio, kept in memory only so it can be played back
  // next to the feedback. Dropped with the prompt.
  const [lastPcm, setLastPcm] = useState<Uint8Array | null>(null);
  // A saved phrase is a new card every time, so the button saves once per prompt.
  const [savedPromptId, setSavedPromptId] = useState<string | null>(null);
  const savingRef = useRef(false);
  const timer = useRef<ReturnType<typeof setInterval> | undefined>(undefined);

  // The Lab's one irreplaceable feature: checking arbitrary writing, not
  // just a transcript. Kept small and out of the way in a <details> panel so
  // it does not compete with the practice flow above it.
  const [checkText, setCheckText] = useState("");
  const [checkLint, setCheckLint] = useState<LintReport | null>(null);
  const [checking, setChecking] = useState(false);
  const checkingRef = useRef(false);
  // Newest request wins: a double-click on Skip leaves two nextPrompt calls in
  // flight, and the slower one must not bring back a prompt already replaced.
  const promptSeq = useRef(0);
  // Read through a ref by `loadPrompt`, so picking a level asks for a new
  // prompt without reopening the session (which would forget what was done).
  const [levelFilter, setLevelFilter] = useState<number | null>(null);
  const levelRef = useRef<number | null>(null);
  // The backend answered and had nothing to offer, as opposed to still loading.
  const [noPrompt, setNoPrompt] = useState(false);

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

  // Show a specific prompt (one the user just wrote or picked from their own
  // list). Bumps the sequence so a slow random-prompt reply cannot replace it.
  const showPrompt = useCallback((p: PromptView) => {
    promptSeq.current += 1;
    setError(null);
    setReport(null);
    setLastPcm(null);
    setStage("prompt");
    setPrompt(p);
    setNoPrompt(false);
  }, []);

  const loadPrompt = useCallback(
    async (session: string | undefined, cat: string) => {
      const seq = ++promptSeq.current;
      setError(null);
      setReport(null);
      setLastPcm(null);
      setStage("prompt");
      try {
        const lvl = levelRef.current;
        const next = await ipc().nextPrompt({
          ...(session === undefined ? {} : { sessionId: session }),
          category: cat,
          ...(lvl === null ? {} : { level: lvl }),
        });
        if (seq !== promptSeq.current) return;
        setPrompt(next);
        setNoPrompt(next === null);
        if (next) announce(next.prompt_text);
      } catch (e) {
        if (seq !== promptSeq.current) return;
        setError(String(e));
      }
    },
    [announce],
  );

  useEffect(() => {
    let live = true;
    void ipc()
      .getPreferences()
      .then((p) => {
        if (!live) return;
        setCategory(categoryForGoal(p.goal));
        setDailyGoal(p.practice_goal_attempts);
        micRef.current = p.mic_device_id;
      })
      .catch(() => {
        // Unreadable preferences: keep the default category.
      })
      .finally(() => {
        if (live) setGoalLoaded(true);
      });
    void ipc()
      .statsOverview(tzOffsetMinutes())
      .then((o) => {
        if (live) setAttemptsToday(o.attempts_today);
      })
      .catch(() => {
        // Only the daily counter is lost.
      });
    return () => {
      live = false;
    };
  }, []);

  // Open a session per category, so "skip what I already did" is scoped to
  // the thing actually being practised.
  useEffect(() => {
    if (!goalLoaded) return;
    let live = true;
    let startedId: string | undefined;
    setSummary(null);
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
        startedId = id;
        if (!live) {
          void ipc().endSession(id).catch(() => {});
          return;
        }
        currentSession.current = id;
        setSessionId(id);
        await loadPrompt(id, category);
      } catch (e) {
        if (live) setError(String(e));
      }
    })();
    return () => {
      live = false;
      currentSession.current = undefined;
      // Close the session it opened; best effort, the next launch is unaffected.
      if (startedId !== undefined) void ipc().endSession(startedId).catch(() => {});
    };
  }, [category, goalLoaded, loadPrompt]);

  // Never leave the microphone open when the route unmounts.
  useEffect(() => {
    const active = recorder.current;
    const clips = player.current;
    return () => {
      active.cancel();
      clips.stop();
      clearInterval(timer.current);
    };
  }, []);

  const stopAndScore = useCallback(async () => {
    clearInterval(timer.current);
    if (!recorder.current.isRecording()) return;
    setStage("scoring");
    try {
      const rec = await recorder.current.stop();
      setLastPcm(rec.pcm);
      setMicNote(describeMicLevel(rec.peak));
      const report = await ipc().scoreAttempt({
        pcm: rec.pcm,
        sampleRate: 16000,
        ...(sessionId === undefined ? {} : { sessionId }),
        ...(prompt ? { promptId: prompt.id } : {}),
        ...(prompt?.target_text ? { targetText: prompt.target_text } : {}),
      });
      setChange(describeChange(reportRef.current, report));
      setReport(report);
      setStage("feedback");
      setAttemptsToday((n) => n + 1);
      if (sessionId !== undefined) {
        void ipc()
          .sessionSummary(sessionId)
          .then((s) => {
            if (currentSession.current === sessionId) setSummary(s);
          })
          .catch(() => {
            // The summary is a nicety; the attempt itself already succeeded.
          });
      }
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
    // The microphone would otherwise pick up the prompt or the last attempt.
    player.current.stop();
    setError(null);
    setElapsedMs(0);
    try {
      setMicNote(null);
      await recorder.current.start(setLevel, micRef.current);
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
      setError(friendlyMicError(e));
    }
  }, [announce, stopAndScore]);

  const speakPrompt = useCallback(async () => {
    const text = prompt?.target_text ?? prompt?.prompt_text;
    if (!text || speakingRef.current) return;
    speakingRef.current = true;
    try {
      await speak(text, player.current);
    } catch (e) {
      setError(friendlyTtsError(e));
    } finally {
      speakingRef.current = false;
    }
  }, [prompt]);

  const playRecording = useCallback(async () => {
    if (!lastPcm) return;
    try {
      await player.current.play([f32ToWav(lastPcm, 16000)]);
    } catch (e) {
      setError(`Could not play your recording: ${String(e)}`);
    }
  }, [lastPcm]);

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
    if (!prompt?.target_text || savingRef.current || savedPromptId === prompt.id) return;
    savingRef.current = true;
    try {
      await ipc().addCard("default", prompt.target_text, prompt.prompt_text);
      setSavedPromptId(prompt.id);
      announce("Saved to your review deck.");
    } catch (e) {
      setError(String(e));
    } finally {
      savingRef.current = false;
    }
  }, [announce, prompt, savedPromptId]);

  // An open interview question: no pronunciation score, but the length of
  // the answer is worth a word of advice.
  const isInterviewAnswer = prompt?.category === "interview" && !prompt.target_text;
  const lengthNote =
    report && prompt ? answerLengthNote(report.duration_ms, prompt.level, isInterviewAnswer) : null;

  return (
    <section class="route">
      <header class="route-head">
        <h1 tabIndex={-1}>Practice</h1>
        {describeDailyGoal(attemptsToday, dailyGoal) && (
          <span class="muted daily-goal">{describeDailyGoal(attemptsToday, dailyGoal)}</span>
        )}
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
      <div class="row practice-filters">
        <div class="segmented" role="group" aria-label="Level">
          {LEVELS.map((l) => (
            <button
              key={String(l.id)}
              type="button"
              aria-pressed={levelFilter === l.id}
              disabled={stage === "recording" || stage === "scoring"}
              onClick={() => {
                if (levelRef.current === l.id) return;
                levelRef.current = l.id;
                setLevelFilter(l.id);
                void loadPrompt(sessionId, category);
              }}
            >
              {l.label}
            </button>
          ))}
        </div>
      </div>

      {!asrReady && (
        <p class="notice notice-blocking">
          The speech model is not installed yet, so recordings cannot be scored.
          Install it and reopen this screen.
        </p>
      )}
      {error && (
        <p class="notice notice-error" role="alert">
          {error}
        </p>
      )}

      {!prompt && !error && !noPrompt && <p class="muted">Loading a prompt…</p>}
      {noPrompt && (
        <p class="muted">
          No prompts here{levelFilter === null ? "" : ` at level ${levelFilter}`}. Pick another
          level or category.
        </p>
      )}

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
          {isInterviewAnswer && (
            <p class="muted">Aim for about {targetSeconds(prompt.level)} seconds.</p>
          )}
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
              <FlagNote words={report.pron.words} />
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

          {micNote && <p class="notice mic-note">{micNote}</p>}
          {change && <p class="muted try-change">{change}</p>}
          {lengthNote && <p class="muted answer-length">{lengthNote}</p>}
          {summary && describeSession(summary) && (
            <p class="muted session-summary">{describeSession(summary)}</p>
          )}

          <div class="row">
            <button type="button" class="primary" onClick={() => void loadPrompt(sessionId, category)}>
              Next prompt
            </button>
            {lastPcm && (
              <button type="button" onClick={() => void playRecording()}>
                Play my recording
              </button>
            )}
            {prompt?.target_text &&
              (savedPromptId === prompt.id ? (
                <button type="button" disabled>
                  Saved to review
                </button>
              ) : (
                <button type="button" onClick={() => void saveToReview()}>
                  Save phrase to review
                </button>
              ))}
          </div>
        </article>
      )}

      <OwnPrompts
        category={category}
        categoryLabel={CATEGORIES.find((c) => c.id === category)?.label ?? category}
        onPractise={showPrompt}
        disabled={stage === "recording" || stage === "scoring"}
        announce={announce}
      />

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
