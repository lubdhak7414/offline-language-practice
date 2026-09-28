import { render } from "@testing-library/preact";
import { describe, expect, it } from "vitest";

import { expectNoA11yViolations } from "./axe";

describe("expectNoA11yViolations", () => {
  it("passes markup with no findings", async () => {
    const { container } = render(
      <main>
        <h1>Title</h1>
        <label>
          Name <input type="text" />
        </label>
      </main>,
    );
    await expectNoA11yViolations(container);
  });

  it("fails with the rule id, its help text and the offending markup", async () => {
    const { container } = render(
      <main>
        <h1>Title</h1>
        <input type="text" />
      </main>,
    );
    await expect(expectNoA11yViolations(container)).rejects.toThrow(
      /label \(critical\): Form elements must have labels\n\s+<input type="text">/,
    );
  });

  it("trims a long offending node to 120 characters", async () => {
    const { container } = render(
      <main>
        <h1>Title</h1>
        <img src="a.png" data-note={"x".repeat(300)} />
      </main>,
    );
    const err = await expectNoA11yViolations(container).then(
      () => null,
      (e: Error) => e,
    );
    expect(err).not.toBeNull();
    const snippet = err!.message.split("\n").find((l) => l.includes("<img"));
    expect(snippet).toBeDefined();
    expect(snippet!.trim().length).toBeLessThanOrEqual(120);
  });

  it("does not try to judge colour contrast, which jsdom cannot compute", async () => {
    const { container } = render(
      <main>
        <h1 style={{ color: "#ccc", background: "#fff" }}>Faint title</h1>
      </main>,
    );
    await expectNoA11yViolations(container);
  });
});
