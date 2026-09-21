import { byteSlice } from "../lib/text/byteSlice";
import type { LintDiagnostic, WordOp } from "../ipc/types";

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
