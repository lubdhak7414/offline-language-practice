import { useCallback, useEffect, useRef, useState } from "preact/hooks";

import { ipc } from "../ipc/commands";
import type { DueCard, Rating } from "../ipc/types";
import { formatInterval } from "../lib/interval";
import { goPrefix } from "../lib/globalKeys";
import { reviewKeyAction } from "../lib/keyboard";

/**
 * How many due cards to fetch at once.
 *
 * The old harness asked for one card at a time, which made "how much is
 * left" unanswerable and cost a round trip per grade.
 */
export const REVIEW_QUEUE_SIZE = 20;

const GRADES: Array<{ rating: Rating; label: string; hint: string }> = [
  { rating: 1, label: "Again", hint: "No idea" },
  { rating: 2, label: "Hard", hint: "Struggled" },
  { rating: 3, label: "Good", hint: "Got it" },
  { rating: 4, label: "Easy", hint: "Instant" },
];

type Tally = Record<Rating, number>;

const EMPTY_TALLY: Tally = { 1: 0, 2: 0, 3: 0, 4: 0 };

export function Review(props: { announce: (msg: string) => void }) {
  const { announce } = props;
  const [queue, setQueue] = useState<DueCard[]>([]);
  const [index, setIndex] = useState(0);
  const [revealed, setRevealed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [tally, setTally] = useState<Tally>(EMPTY_TALLY);
  const [log, setLog] = useState<string[]>([]);
  const goodButton = useRef<HTMLButtonElement>(null);
  // A ref, not the `busy` state, because two clicks in the same tick both
  // read the state from their own closure and both see `false`. The button
  // going disabled is the visible half of the guard; this is the real one.
  const grading = useRef(false);

  const card = queue[index];
  const finished = !loading && !card;

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const cards = await ipc().dueCards({ limit: REVIEW_QUEUE_SIZE });
      setQueue(cards);
      setIndex(0);
      setRevealed(false);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const reveal = useCallback(() => {
    setRevealed(true);
  }, []);

  // Moving focus to Good after a reveal puts the most common grade under the
  // hand. The keyboard rule deliberately still accepts 1-4 with a button
  // focused, which is the bug this pairing used to hide.
  useEffect(() => {
    if (revealed) goodButton.current?.focus();
  }, [revealed]);

  const grade = useCallback(
    async (rating: Rating) => {
      // Without this guard a double-click inserts two review_logs rows and
      // the second one trains the optimizer on a zero-day interval.
      if (!card || grading.current || !revealed) return;
      grading.current = true;
      setBusy(true);
      try {
        const replacement = await ipc().gradeCard(card.id, rating);
        const label = GRADES.find((g) => g.rating === rating)?.label ?? "";
        setTally((t) => ({ ...t, [rating]: t[rating] + 1 }));
        setLog((l) => [...l, `${label}: ${firstLine(card.front)}`]);
        announce(`${label}. Next in ${formatInterval(card.intervals[String(rating)])}.`);
        setQueue((q) =>
          // Only extend at the end of the queue: mid-queue the backend's
          // "next due" is almost always the card already sitting at index+1,
          // and appending it would show it twice.
          index === q.length - 1 && replacement ? [...q, replacement] : q,
        );
        setIndex((i) => i + 1);
        setRevealed(false);
      } catch (e) {
        setError(String(e));
      } finally {
        grading.current = false;
        setBusy(false);
      }
    },
    [announce, card, index, revealed],
  );

  // The handler reads through a ref rather than closing over state, so the
  // listener is registered exactly once. Re-registering it per render means
  // there is a window after every state change where the attached listener
  // still believes the previous state — long enough to swallow a keystroke.
  const latest = useRef({ card, revealed, reveal, grade });
  latest.current = { card, revealed, reveal, grade };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const now = latest.current;
      const target = e.target as HTMLElement | null;
      const action = reviewKeyAction(e.key, {
        targetTag: target?.tagName ?? "",
        isContentEditable: target?.isContentEditable ?? false,
        isComposing: e.isComposing,
        hasCard: Boolean(now.card),
        revealed: now.revealed,
        gradeRowHidden: !now.revealed,
        revealHidden: now.revealed,
        goPending: goPrefix.armed,
      });
      if (!action) return;
      e.preventDefault();
      if (action.kind === "reveal") now.reveal();
      else void now.grade(action.rating);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);

  const reviewed = tally[1] + tally[2] + tally[3] + tally[4];

  return (
    <section class="route review">
      <div class="route-head">
        <h1 tabIndex={-1}>Review</h1>
        {card && (
          <p class="muted" aria-live="off">
            {index + 1} of {queue.length}
          </p>
        )}
      </div>

      {error && (
        <p class="notice notice-error" role="alert">
          {error}
        </p>
      )}

      {loading && <p class="muted">Looking for cards…</p>}

      {card && (
        <article class="review-card">
          {card.deck_name && <p class="chip">{card.deck_name}</p>}
          <p class="review-front">{card.front}</p>
          {revealed ? (
            <p class="review-back">{card.back}</p>
          ) : (
            <button type="button" class="primary" onClick={reveal}>
              Show answer <kbd>Space</kbd>
            </button>
          )}

          {revealed && (
            <div class="grade-row">
              {GRADES.map((g) => (
                <button
                  key={g.rating}
                  type="button"
                  ref={g.rating === 3 ? goodButton : null}
                  class={`grade grade-${g.label.toLowerCase()}`}
                  disabled={busy}
                  onClick={() => void grade(g.rating)}
                >
                  <span class="grade-key">{g.rating}</span>
                  <span class="grade-label">{g.label}</span>
                  <span class="grade-interval">
                    {formatInterval(card.intervals[String(g.rating)])}
                  </span>
                  <span class="grade-hint">{g.hint}</span>
                </button>
              ))}
            </div>
          )}
        </article>
      )}

      {finished && (
        <article class="review-summary">
          <h2>{reviewed > 0 ? "Round finished" : "Nothing due"}</h2>
          {reviewed > 0 ? (
            <>
              <p>
                You reviewed {reviewed} {reviewed === 1 ? "card" : "cards"}.
              </p>
              <ul class="tally">
                {GRADES.map((g) => (
                  <li key={g.rating}>
                    {g.label}: {tally[g.rating]}
                  </li>
                ))}
              </ul>
            </>
          ) : (
            <p class="muted">
              Nothing is due right now. Save a phrase from Practice, or come
              back when today's cards come round.
            </p>
          )}
          <button type="button" class="primary" onClick={() => void load()}>
            Check again
          </button>
        </article>
      )}

      {/*
        The app's one `role="log"`, beside the one toast in `App`. A log is
        appended to rather than replaced, which is what a running history
        actually is — a status region would re-announce the whole thing.
      */}
      <div class="review-log" role="log" aria-label="This session">
        {log.slice(-5).map((entry, i) => (
          <p key={i} class="muted">
            {entry}
          </p>
        ))}
      </div>
    </section>
  );
}

/** Cards can hold a paragraph; a log line holds the first sentence of it. */
function firstLine(text: string): string {
  const line = text.split("\n")[0] ?? "";
  return line.length > 60 ? `${line.slice(0, 57)}…` : line;
}
