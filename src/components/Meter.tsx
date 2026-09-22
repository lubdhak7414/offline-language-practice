import { byteSlice } from "../lib/text/byteSlice";
import type {
  FluencyReport,
  LintDiagnostic,
  WordOp,
  WordScore,
} from "../ipc/types";

/**
 * A single score with a plain-English verdict.
 *
 * `value === null` renders the reason it was not measured rather than a
 * placeholder number. A greyed-out "0" and a genuine zero are impossible to
 * tell apart, and one of them is a lie.
 */
export function Meter(props: {
  label: string;
  value: number | null;
  /** Shown in place of the bar when `value` is null. */
  unmeasuredNote?: string;
}) {
  const { label, value, unmeasuredNote } = props;
  if (value === null) {
    return (
      <div class="meter meter-unmeasured">
        <div class="meter-label">{label}</div>
        <div class="meter-note">{unmeasuredNote ?? "Not scored"}</div>
      </div>
    );
  }
  return (
    <div class="meter" data-band={band(value)}>
      <div class="meter-label">{label}</div>
      <div class="meter-value">{value}</div>
      <div
        class="meter-bar"
        role="meter"
        aria-valuenow={value}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-label={`${label}: ${value} out of 100, ${verdict(value)}`}
      >
        <span style={{ width: `${value}%` }} />
      </div>
      <div class="meter-note">{verdict(value)}</div>
    </div>
  );
}

function band(value: number): "low" | "mid" | "high" {
  if (value >= 80) return "high";
  if (value >= 55) return "mid";
  return "low";
}

function verdict(value: number): string {
  if (value >= 90) return "Clear";
  if (value >= 75) return "Mostly clear";
  if (value >= 55) return "Understandable";
  if (value >= 30) return "Hard to follow";
  return "Needs work";
}

/** The target sentence, with each word marked by how it was said. */
export function WordAlignmentView(props: { ops: WordOp[] }) {
  return (
    <p class="alignment" aria-label="Word by word comparison">
      {props.ops.map((op, i) => {
        switch (op.kind) {
          case "match":
            return (
              <span key={i} class="word word-match">
                {op.word}
              </span>
            );
          case "sub":
            return (
              <span key={i} class="word word-sub" title={`You said ${op.spoken}`}>
                {op.expected}
                <span class="word-said"> ({op.spoken})</span>
              </span>
            );
          case "del":
            return (
              <span key={i} class="word word-del" title="Not said">
                {op.word}
              </span>
            );
          case "ins":
            return (
              <span key={i} class="word word-ins" title="Extra word">
                {op.word}
              </span>
            );
        }
      })}
    </p>
  );
}

/**
 * The target sentence again, this time with how clearly each word was said.
 *
 * Shown only when acoustic scoring ran. It answers a different question from
 * the alignment above — that one says which word was wrong, this one says
 * which word was mumbled.
 */
export function WordScoreView(props: { words: WordScore[] }) {
  return (
    <p class="alignment" aria-label="Word by word pronunciation">
      {props.words.map((w, i) => (
        <span
          key={i}
          class={`word word-gop word-${w.verdict}`}
          title={`${w.word}: ${w.score} out of 100`}
        >
          {w.word}
          <span class="word-score">{w.score}</span>
        </span>
      ))}
    </p>
  );
}

/**
 * Delivery in one line of plain English.
 *
 * Rate is reported as articulation rate — words per minute of actual
 * speaking — because that is the number a learner can act on. Overall wpm
 * drops when someone pauses to think, which is not a speaking-speed problem.
 */
export function DeliveryNote(props: { fluency: FluencyReport }) {
  const f = props.fluency;
  const bits = [`${Math.round(f.articulation_wpm)} words a minute while speaking`];
  if (f.pause_count > 0) {
    bits.push(
      `${f.pause_count} ${plural(f.pause_count, "pause")}, longest ${(
        f.longest_pause_ms / 1000
      ).toFixed(1)}s`,
    );
  }
  const fillers = f.filler_count + f.hesitation_count;
  if (fillers > 0) bits.push(`${fillers} ${plural(fillers, "filler")}`);
  return (
    <>
      <p class="muted">{bits.join(" · ")}</p>
      {f.like_count > 0 && (
        <p class="muted">
          You said "like" {f.like_count} {plural(f.like_count, "time")} — some of
          those are probably the ordinary word, so this one is a hint, not a
          count.
        </p>
      )}
    </>
  );
}

function plural(n: number, word: string): string {
  return n === 1 ? word : `${word}s`;
}

/**
 * Grammar issues, underlined in place in the transcript.
 *
 * Spans arrive as UTF-8 byte offsets, so every slice goes through
 * `byteSlice` — see the note there for why.
 */
export function LintedText(props: { text: string; diags: LintDiagnostic[] }) {
  const { text, diags } = props;
  if (diags.length === 0) return <p class="transcript">{text}</p>;

  const sorted = [...diags].sort((a, b) => a.start - b.start);
  const parts: preact.ComponentChildren[] = [];
  let cursor = 0;
  sorted.forEach((d, i) => {
    if (d.start < cursor) return; // overlapping span: keep the first
    parts.push(byteSlice(text, cursor, d.start));
    parts.push(
      <mark
        key={i}
        class={`lint-err lint-sev-${d.severity}`}
        title={`${d.message}${d.suggestions.length ? ` — try: ${d.suggestions.join(", ")}` : ""}`}
      >
        {byteSlice(text, d.start, d.end)}
      </mark>,
    );
    cursor = d.end;
  });
  parts.push(byteSlice(text, cursor, Number.MAX_SAFE_INTEGER));
  return <p class="transcript">{parts}</p>;
}
