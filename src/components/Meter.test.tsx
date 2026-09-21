import { render, screen } from "@testing-library/preact";
import { describe, expect, it } from "vitest";

import { LintedText, Meter, WordAlignmentView } from "./Meter";
import type { WordOp } from "../ipc/types";

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
