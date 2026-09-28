import { render, screen, within } from "@testing-library/preact";
import { afterEach, describe, expect, it } from "vitest";

import { setIpc } from "../ipc/commands";
import { createMockIpc, type MockIpc } from "../ipc/mock";
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
      expect(mock.calls.find((c) => c.name === "listAttempts")?.args).toEqual([undefined, 20]);
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
          { day: day(0), attempts: 2, avg_pron: 70, avg_wpm: 120 },
          { day: day(1), attempts: 0, avg_pron: null, avg_wpm: null },
          { day: day(2), attempts: 1, avg_pron: null, avg_wpm: 100 },
          { day: day(3), attempts: 3, avg_pron: 82.4, avg_wpm: 130 },
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
