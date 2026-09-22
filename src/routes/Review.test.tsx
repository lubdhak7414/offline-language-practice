import { fireEvent, render, screen, waitFor } from "@testing-library/preact";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";

import { setIpc } from "../ipc/commands";
import { createMockIpc, makeDueCard, type MockIpc } from "../ipc/mock";
import { Review } from "./Review";

let restore: (() => void) | undefined;

function mount(mock: MockIpc) {
  restore = setIpc(mock);
  return render(<Review announce={() => {}} />);
}

afterEach(() => {
  restore?.();
  restore = undefined;
});

const twoCards = () =>
  createMockIpc({
    due: [
      makeDueCard({ id: "c1", front: "first front", back: "first back" }),
      makeDueCard({ id: "c2", front: "second front", back: "second back" }),
    ],
  });

describe("Review", () => {
  it("shows the front and keeps the answer hidden until asked", async () => {
    mount(twoCards());
    expect(await screen.findByText("first front")).toBeInTheDocument();
    expect(screen.queryByText("first back")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Good/ })).not.toBeInTheDocument();
  });

  it("counts the queue so the session has an end in sight", async () => {
    mount(twoCards());
    expect(await screen.findByText("1 of 2")).toBeInTheDocument();
  });

  it("reveals on Space", async () => {
    mount(twoCards());
    await screen.findByText("first front");
    fireEvent.keyDown(document, { key: " " });
    expect(await screen.findByText("first back")).toBeInTheDocument();
  });

  it("grades from the number keys while a grade button holds focus", async () => {
    // The regression this route exists to not repeat: revealing focuses a
    // button, and the old handler ignored every key whose target was one.
    const mock = twoCards();
    mount(mock);
    await screen.findByText("first front");
    fireEvent.keyDown(document, { key: " " });
    const good = await screen.findByRole("button", { name: /Good/ });
    await waitFor(() => expect(document.activeElement).toBe(good));

    fireEvent.keyDown(good, { key: "3" });
    await waitFor(() =>
      expect(mock.calls.filter((c) => c.name === "gradeCard")).toHaveLength(1),
    );
    expect(mock.calls.find((c) => c.name === "gradeCard")?.args).toEqual(["c1", 3]);
    expect(await screen.findByText("second front")).toBeInTheDocument();
  });

  it("refuses to grade a card that has not been revealed", async () => {
    const mock = twoCards();
    mount(mock);
    await screen.findByText("first front");
    fireEvent.keyDown(document, { key: "3" });
    await new Promise((r) => setTimeout(r, 0));
    expect(mock.calls.some((c) => c.name === "gradeCard")).toBe(false);
  });

  it("shows what each grade schedules", async () => {
    mount(twoCards());
    await screen.findByText("first front");
    fireEvent.keyDown(document, { key: " " });
    const good = await screen.findByRole("button", { name: /Good/ });
    // intervals["3"] is 4.6 days in the fixture.
    expect(good).toHaveTextContent("5d");
  });

  it("counts a double-click as one review", async () => {
    const mock = twoCards();
    mount(mock);
    await screen.findByText("first front");
    fireEvent.keyDown(document, { key: " " });
    const good = await screen.findByRole("button", { name: /Good/ });
    fireEvent.click(good);
    fireEvent.click(good);
    await waitFor(() => screen.getByText("second front"));
    expect(mock.calls.filter((c) => c.name === "gradeCard")).toHaveLength(1);
  });

  it("ends with a summary of what was graded", async () => {
    const user = userEvent.setup();
    mount(
      createMockIpc({
        due: [makeDueCard({ id: "only", front: "only front", back: "only back" })],
      }),
    );
    await screen.findByText("only front");
    fireEvent.keyDown(document, { key: " " });
    await user.click(await screen.findByRole("button", { name: /Good/ }));
    expect(await screen.findByText("Round finished")).toBeInTheDocument();
    expect(screen.getByText("You reviewed 1 card.")).toBeInTheDocument();
    expect(screen.getByText("Good: 1")).toBeInTheDocument();
  });

  it("says so plainly when nothing is due", async () => {
    mount(createMockIpc({ due: [] }));
    expect(await screen.findByText("Nothing due")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /Show answer/ }),
    ).not.toBeInTheDocument();
  });

  it("keeps one log region for the session history", async () => {
    mount(twoCards());
    await screen.findByText("first front");
    fireEvent.keyDown(document, { key: " " });
    fireEvent.click(await screen.findByRole("button", { name: /Good/ }));
    const log = await screen.findByRole("log");
    await waitFor(() => expect(log).toHaveTextContent("Good: first front"));
    expect(screen.getAllByRole("log")).toHaveLength(1);
  });
});
