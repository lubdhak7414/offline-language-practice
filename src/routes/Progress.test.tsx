import { render, screen, within } from "@testing-library/preact";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";

import { takePracticeRequest } from "../app/handoff";

import { setIpc } from "../ipc/commands";
import type { PracticeDay } from "../ipc/types";
import { createMockIpc, type MockIpc } from "../ipc/mock";
import { expectNoA11yViolations } from "../test/axe";
import { Progress } from "./Progress";

let restore: (() => void) | undefined;

function mount(mock: MockIpc) {
  restore = setIpc(mock);
  return render(<Progress />);
}

afterEach(() => {
  restore?.();
  restore = undefined;
});

describe("Progress", () => {
  it("shows the overview tiles", async () => {
    mount(
      createMockIpc({
        overview: {
          total_reviews: 214,
          reviews_today: 12,
          streak_days: 6,
          attempts_today: 0,
          cards_total: 10,
          cards_new: 2,
          cards_learning: 1,
          cards_mature: 7,
          retention_30d: 0.91,
          practice_ms_30d: 5_400_000,
          attempts_total: 48,
        },
      }),
    );
    expect(await screen.findByText("6 days")).toBeInTheDocument();
    expect(screen.getByText("214")).toBeInTheDocument();
    expect(screen.getByText("91%")).toBeInTheDocument();
    expect(screen.getByText("90 min")).toBeInTheDocument();
  });

  it("says so plainly rather than showing a fabricated retention", async () => {
    mount(
      createMockIpc({
        overview: {
          total_reviews: 0,
          reviews_today: 0,
          streak_days: 0,
          attempts_today: 0,
          cards_total: 0,
          cards_new: 0,
          cards_learning: 0,
          cards_mature: 0,
          retention_30d: null,
          practice_ms_30d: 0,
          attempts_total: 0,
        },
      }),
    );
    expect(await screen.findByText("Not enough data")).toBeInTheDocument();
  });

  it("renders the three charts", async () => {
    mount(createMockIpc());
    expect(
      await screen.findByRole("img", { name: /Reviews per day \(last 30 days\)/ }),
    ).toBeInTheDocument();
    expect(screen.getByRole("img", { name: /Due forecast \(next 14 days\)/ })).toBeInTheDocument();
    expect(screen.getByRole("img", { name: /Retention by week/ })).toBeInTheDocument();
  });

  describe("recent practice", () => {
    const row = {
      id: "a1",
      prompt_id: "p1",
      target_text: "Hi, good to see you again.",
      transcript: "hi good to see you again",
      duration_ms: 2400,
      created_at: Date.UTC(2026, 8, 27, 14, 5),
      pron_overall: 82,
      pron_method: "gop",
      overall: 88,
    };

    it("offers Practise again for a prompt and hands its id to Practice", async () => {
      const user = userEvent.setup();
      mount(createMockIpc({ attempts: [row, { ...row, id: "a2", prompt_id: null }] }));
      const section = await screen.findByRole("region", { name: "Recent practice" });
      const buttons = await within(section).findAllByRole("button", { name: "Practise again" });
      expect(buttons).toHaveLength(1);
      await user.click(buttons[0] as HTMLElement);
      expect(takePracticeRequest()).toBe("p1");
      expect(takePracticeRequest()).toBeNull();
    });

    it("lists attempts with their scores, newest first", async () => {
      const mock = createMockIpc({
        attempts: [
          row,
          {
            ...row,
            id: "a2",
            prompt_id: null,
            target_text: null,
            transcript: "I would say my biggest strength is staying calm when things go wrong at work",
            pron_overall: null,
            pron_method: null,
            overall: 71,
          },
          { ...row, id: "a3", pron_method: "text", pron_overall: 100 },
        ],
      });
      mount(mock);
      const section = await screen.findByRole("region", { name: "Recent practice" });
      const table = await within(section).findByRole("table");
      const rows = within(table).getAllByRole("row").slice(1);
      expect(rows).toHaveLength(3);
      expect(rows[0]).toHaveTextContent("Hi, good to see you again.");
      expect(rows[0]).toHaveTextContent("88");
      expect(rows[0]).toHaveTextContent("82");
      expect(rows[0]).toHaveTextContent("2.4s");
      // Free speaking: the start of what was said, and no invented score.
      expect(rows[1]).toHaveTextContent("Free speaking: I would say my biggest strength is staying calm when things…");
      expect(rows[1]).toHaveTextContent("Not scored");
      expect(rows[2]).toHaveTextContent("100 (word matching)");
      expect(mock.calls.find((c) => c.name === "listAttempts")?.args).toEqual([undefined, 200]);
    });

    it("pages through a long history and can show one whole day", async () => {
      const user = userEvent.setup();
      const day = (n: number, h: number) => new Date(2026, 8, 28 - n, h).getTime();
      // 25 attempts on the 28th and 3 on the 27th, newest first.
      const many = [
        ...Array.from({ length: 25 }, (_, i) => ({ ...row, id: `t${i}`, created_at: day(0, 23) - i * 60_000 })),
        ...Array.from({ length: 3 }, (_, i) => ({ ...row, id: `y${i}`, created_at: day(1, 12) - i * 60_000 })),
      ];
      mount(createMockIpc({ attempts: many }));
      const section = await screen.findByRole("region", { name: "Recent practice" });
      const count = () => within(section).getAllByRole("row").length - 1;
      await within(section).findByRole("table");
      expect(count()).toBe(20);

      await user.click(within(section).getByRole("button", { name: "Show 8 more" }));
      expect(count()).toBe(28);
      expect(within(section).queryByRole("button", { name: /more$/ })).not.toBeInTheDocument();

      await user.selectOptions(within(section).getByRole("combobox", { name: "Show" }), "2026-9-27");
      expect(count()).toBe(3);
      await user.selectOptions(within(section).getByRole("combobox", { name: "Show" }), "");
      expect(count()).toBe(28);
    });

    it("offers no day picker when all attempts fall on one day", async () => {
      mount(createMockIpc({ attempts: [row, { ...row, id: "a2" }] }));
      const section = await screen.findByRole("region", { name: "Recent practice" });
      await within(section).findByRole("table");
      expect(within(section).queryByRole("combobox")).not.toBeInTheDocument();
    });

    it("says there is no practice yet", async () => {
      mount(createMockIpc());
      expect(await screen.findByText(/No practice yet/)).toBeInTheDocument();
    });

    it("keeps the charts when the history fails to load", async () => {
      mount(createMockIpc({ fail: { listAttempts: "db locked" } }));
      expect(await screen.findByText(/Could not load your practice history: db locked/)).toBeInTheDocument();
      expect(screen.getAllByText("Reviews per day (last 30 days)").length).toBeGreaterThan(0);
    });
  });

  it("draws pronunciation only for days that had a score, and says why others are missing", async () => {
    const day = (n: number) => 1_700_000_000 + n * 86_400;
    mount(
      createMockIpc({
        practice: [
          { day: day(0), attempts: 2, scored: 2, avg_pron: 70, avg_wpm: 120 },
          { day: day(1), attempts: 0, scored: 0, avg_pron: null, avg_wpm: null },
          { day: day(2), attempts: 1, scored: 0, avg_pron: null, avg_wpm: 100 },
          { day: day(3), attempts: 3, scored: 3, avg_pron: 82.4, avg_wpm: 130 },
        ],
      }),
    );
    const [caption] = await screen.findAllByText("Pronunciation on days you read aloud");
    const chart = caption?.closest("figure") as HTMLElement;
    const rows = within(chart).getAllByRole("row");
    // header + the two scored days; the free-speaking day and the empty day are gaps.
    expect(rows).toHaveLength(3);
    expect(within(chart).getByText("82")).toBeInTheDocument();
    expect(screen.getByText(/Days without a read-aloud attempt are left out/)).toBeInTheDocument();
  });
});

describe("Progress weekly recap", () => {
  const blank: PracticeDay = { day: 0, attempts: 0, scored: 0, avg_pron: null, avg_wpm: null };
  const days = (last7: Array<Partial<PracticeDay>>): PracticeDay[] =>
    Array.from({ length: 14 }, (_, i) => ({ ...blank, day: i * 86_400, ...(i >= 7 ? last7[i - 7] : {}) }));

  it("sums up the last seven days above the charts", async () => {
    mount(createMockIpc({ practice: days([{ attempts: 2, scored: 2, avg_pron: 70 }, { attempts: 1 }]) }));
    expect(await screen.findByText(/In the last 7 days you did 3 speaking attempts on 2 days/)).toBeInTheDocument();
    expect(screen.getByText(/averaged 70 over 2 scored attempts/)).toBeInTheDocument();
  });

  it("is absent when nothing was practised this week", async () => {
    mount(createMockIpc({ practice: days([]) }));
    await screen.findByRole("img", { name: /Reviews per day/ });
    expect(screen.queryByText(/In the last 7 days/)).not.toBeInTheDocument();
  });
});

describe("Progress accessibility", () => {
  it("has no violations with charts and no practice yet", async () => {
    const { container } = mount(createMockIpc());
    await screen.findByRole("img", { name: /Reviews per day \(last 30 days\)/ });
    await expectNoA11yViolations(container);
  });

  it("has no violations with recent practice listed", async () => {
    const row = {
      id: "a1",
      prompt_id: "p1",
      target_text: "Hi, good to see you again.",
      transcript: "hi good to see you again",
      duration_ms: 2400,
      created_at: Date.UTC(2026, 8, 27, 14, 5),
      pron_overall: 82,
      pron_method: "gop",
      overall: 88,
    };
    const { container } = mount(
      createMockIpc({
        attempts: [
          row,
          { ...row, id: "a2", prompt_id: null, target_text: null, pron_overall: null, pron_method: null },
          // A second day, so the day picker is on the page too.
          { ...row, id: "a3", created_at: row.created_at - 2 * 86_400_000 },
        ],
      }),
    );
    await screen.findByRole("region", { name: "Recent practice" });
    await within(screen.getByRole("region", { name: "Recent practice" })).findByRole("table");
    await expectNoA11yViolations(container);
  });
});
