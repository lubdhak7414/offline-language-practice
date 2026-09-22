import { render, screen } from "@testing-library/preact";
import { describe, expect, it } from "vitest";

import {
  DeliveryNote,
  FlagNote,
  LintedText,
  Meter,
  WordAlignmentView,
  WordScoreView,
} from "./Meter";
import type { FluencyReport, WordOp, WordScore } from "../ipc/types";

describe("Meter", () => {
  it("shows the score and a plain-English verdict", () => {
    render(<Meter label="Pronunciation" value={92} />);
    expect(screen.getByText("92")).toBeInTheDocument();
    expect(screen.getByText("Clear")).toBeInTheDocument();
    expect(screen.getByRole("meter")).toHaveAttribute("aria-valuenow", "92");
  });

  it("explains an unmeasured score rather than showing a number", () => {
    // A greyed-out 0 and a real 0 are indistinguishable, and one is a lie.
    render(
      <Meter
        label="Pronunciation"
        value={null}
        unmeasuredNote="Not scored for free speaking"
      />,
    );
    expect(screen.getByText("Not scored for free speaking")).toBeInTheDocument();
    expect(screen.queryByRole("meter")).toBeNull();
    expect(screen.queryByText("0")).toBeNull();
  });

  it("bands the score so colour is not the only signal", () => {
    const { container, rerender } = render(<Meter label="x" value={90} />);
    expect(container.querySelector(".meter")).toHaveAttribute("data-band", "high");
    rerender(<Meter label="x" value={20} />);
    expect(container.querySelector(".meter")).toHaveAttribute("data-band", "low");
  });
});

describe("WordAlignmentView", () => {
  const ops: WordOp[] = [
    { kind: "match", hyp_index: 0, target_index: 0, word: "THE" },
    { kind: "sub", hyp_index: 1, target_index: 1, spoken: "HAT", expected: "CAT" },
    { kind: "del", target_index: 2, word: "SAT" },
    { kind: "ins", hyp_index: 2, word: "UM" },
  ];

  it("renders the target word and what was actually said", () => {
    const { container } = render(<WordAlignmentView ops={ops} />);
    expect(screen.getByText("THE")).toBeInTheDocument();
    expect(screen.getByText("CAT")).toBeInTheDocument();
    expect(screen.getByText("(HAT)")).toBeInTheDocument();
    expect(container.querySelectorAll(".word")).toHaveLength(4);
  });

  it("gives each verdict its own class, not just a colour", () => {
    const { container } = render(<WordAlignmentView ops={ops} />);
    for (const cls of ["word-match", "word-sub", "word-del", "word-ins"]) {
      expect(container.querySelector(`.${cls}`)).not.toBeNull();
    }
  });
});

describe("LintedText", () => {
  it("renders plain text when there is nothing to flag", () => {
    render(<LintedText text="all good here" diags={[]} />);
    expect(screen.getByText("all good here")).toBeInTheDocument();
  });

  it("slices spans by UTF-8 byte offsets", () => {
    // "café" is five bytes; the backend's span for "au" is 6..8, which plain
    // String.slice would render one character off.
    const { container } = render(
      <LintedText
        text="café au lait"
        diags={[
          { start: 6, end: 8, message: "check this", suggestions: ["a"], severity: "warning", rule_id: "R" },
        ]}
      />,
    );
    expect(container.querySelector("mark")?.textContent).toBe("au");
  });

  it("drops a span that overlaps an earlier one rather than duplicating text", () => {
    const { container } = render(
      <LintedText
        text="one two three"
        diags={[
          { start: 0, end: 7, message: "a", suggestions: [], severity: "error", rule_id: "R" },
          { start: 4, end: 9, message: "b", suggestions: [], severity: "error", rule_id: "R" },
        ]}
      />,
    );
    expect(container.querySelectorAll("mark")).toHaveLength(1);
    expect(container.textContent).toBe("one two three");
  });
});

const word = (over: Partial<WordScore> = {}): WordScore => ({
  word: "HELLO",
  start_ms: 0,
  end_ms: 400,
  gop: -0.02,
  score: 96,
  verdict: "good",
  ...over,
});

const fluency = (over: Partial<FluencyReport> = {}): FluencyReport => ({
  wpm: 98,
  articulation_wpm: 142,
  longest_pause_ms: 1800,
  pause_count: 2,
  pauses: [],
  filler_count: 3,
  like_count: 0,
  hesitation_count: 1,
  speaking_ms: 4000,
  method: "aligned",
  score: 71,
  ...over,
});

describe("WordScoreView", () => {
  it("marks a flagged word in text, not only colour, and shows no number", () => {
    render(
      <WordScoreView
        words={[word(), word({ word: "THERE", score: 7, verdict: "unclear" })]}
      />,
    );
    expect(screen.getAllByText("check")).toHaveLength(1);
    expect(screen.getByTitle("THERE: worth another listen")).toBeInTheDocument();
    expect(screen.queryByText("96")).not.toBeInTheDocument();
    expect(screen.queryByText("7")).not.toBeInTheDocument();
  });

  it("explains what a flag is worth, or that there are none", () => {
    const { rerender } = render(<FlagNote words={[word()]} />);
    expect(screen.getByText("Every word came through clearly.")).toBeInTheDocument();
    rerender(<FlagNote words={[word(), word({ word: "A", verdict: "unclear" })]} />);
    expect(screen.getByText(/One word is worth another listen/)).toBeInTheDocument();
    expect(screen.getByText(/a hint, not a mistake/)).toBeInTheDocument();
  });

  it("gives each verdict its own class", () => {
    const { container } = render(
      <WordScoreView
        words={[
          word({ verdict: "good" }),
          word({ word: "A", verdict: "unclear" }),
          word({ word: "B", verdict: "poor" }),
        ]}
      />,
    );
    expect(container.querySelectorAll(".word-good")).toHaveLength(1);
    expect(container.querySelectorAll(".word-unclear")).toHaveLength(1);
    expect(container.querySelectorAll(".word-poor")).toHaveLength(1);
  });
});

describe("DeliveryNote", () => {
  it("reports the speaking rate, not the rate diluted by pauses", () => {
    render(<DeliveryNote fluency={fluency()} />);
    // 142 is articulation_wpm; 98 is wpm, which is not what a learner can act on.
    expect(screen.getByText(/142 words a minute/)).toBeInTheDocument();
    expect(screen.queryByText(/98 words a minute/)).not.toBeInTheDocument();
  });

  it("counts pauses and fillers together with the acoustic hesitations", () => {
    render(<DeliveryNote fluency={fluency()} />);
    expect(screen.getByText(/2 pauses, longest 1.8s/)).toBeInTheDocument();
    expect(screen.getByText(/4 fillers/)).toBeInTheDocument();
  });

  it("says nothing about pauses or fillers when there were none", () => {
    render(
      <DeliveryNote
        fluency={fluency({ pause_count: 0, filler_count: 0, hesitation_count: 0 })}
      />,
    );
    expect(screen.queryByText(/pause/)).not.toBeInTheDocument();
    expect(screen.queryByText(/filler/)).not.toBeInTheDocument();
  });

  it("hedges on LIKE instead of counting it as a mistake", () => {
    render(<DeliveryNote fluency={fluency({ like_count: 2 })} />);
    expect(screen.getByText(/hint, not a\s+count/)).toBeInTheDocument();
  });
});
