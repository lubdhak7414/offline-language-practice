import { render, screen } from "@testing-library/preact";
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
});
