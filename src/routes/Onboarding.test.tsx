import { render, screen, waitFor } from "@testing-library/preact";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { setIpc } from "../ipc/commands";
import { createMockIpc, type MockIpc } from "../ipc/mock";
import { Onboarding } from "./Onboarding";

let restore: (() => void) | undefined;

function mount(mock: MockIpc, onFinish = () => {}) {
  restore = setIpc(mock);
  return render(<Onboarding announce={() => {}} onFinish={onFinish} />);
}

afterEach(() => {
  restore?.();
  restore = undefined;
});

/** Click through Welcome and Goal to the model step. */
async function toModelStep(user: ReturnType<typeof userEvent.setup>) {
  await user.click(await screen.findByRole("button", { name: "Get started" }));
  await user.click(await screen.findByRole("button", { name: "Continue" }));
}

describe("Onboarding", () => {
  it("walks Welcome -> Goal -> Models -> Mic", async () => {
    const user = userEvent.setup();
    mount(createMockIpc());

    expect(await screen.findByRole("heading", { name: "Welcome" })).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Get started" }));

    expect(
      await screen.findByRole("heading", { name: "What do you want to practise?" }),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Continue" }));

    expect(
      await screen.findByRole("heading", { name: "Set up your voice" }),
    ).toBeInTheDocument();
  });

  it("downloads the missing models and marks them installed", async () => {
    const user = userEvent.setup();
    const mock = createMockIpc();
    mount(mock);
    await toModelStep(user);

    // Both groups start missing, so the button offers their combined size.
    const button = await screen.findByRole("button", { name: /^Download / });
    await user.click(button);

    await waitFor(() => {
      expect(screen.getAllByText("Installed")).toHaveLength(2);
    });
    expect(mock.calls.some((c) => c.name === "downloadModels")).toBe(true);
  });

  it("offers Continue rather than Skip once nothing is missing", async () => {
    const user = userEvent.setup();
    mount(
      createMockIpc({
        catalog: [
          {
            id: "asr",
            label: "Speech recognition",
            detail: "d",
            bytes: 100,
            installed: true,
          },
        ],
      }),
    );
    await toModelStep(user);
    expect(await screen.findByRole("button", { name: "Continue" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Skip for now" })).toBeNull();
  });

  it("lets someone skip the download and still finish", async () => {
    // Skipping must stay possible: a metered connection is a reason to
    // defer, not a reason to be locked out of the app.
    const user = userEvent.setup();
    const onFinish = vi.fn();
    const mock = createMockIpc();
    mount(mock, onFinish);
    await toModelStep(user);

    await user.click(await screen.findByRole("button", { name: "Skip for now" }));
    expect(
      await screen.findByRole("heading", { name: "Check your microphone" }),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Finish" }));

    await waitFor(() => expect(onFinish).toHaveBeenCalled());
  });

  it("saves the chosen goal and the onboarded flag when it finishes", async () => {
    const user = userEvent.setup();
    const mock = createMockIpc();
    mount(mock);

    await user.click(await screen.findByRole("button", { name: "Get started" }));
    await user.click(await screen.findByRole("radio", { name: /Job interviews/ }));
    await user.click(screen.getByRole("button", { name: "Continue" }));
    await user.click(await screen.findByRole("button", { name: "Skip for now" }));
    await user.click(await screen.findByRole("button", { name: "Finish" }));

    await waitFor(() => {
      const saved = mock.calls.find((c) => c.name === "setPreferences");
      expect(saved).toBeDefined();
      const prefs = saved?.args[0] as { goal: string; onboarded: boolean };
      expect(prefs.goal).toBe("interview");
      expect(prefs.onboarded).toBe(true);
    });
  });

  it("reports a failed download instead of silently doing nothing", async () => {
    const user = userEvent.setup();
    mount(
      createMockIpc({
        fail: { downloadModels: new Error("network unreachable") },
      }),
    );
    await toModelStep(user);
    await user.click(await screen.findByRole("button", { name: /^Download / }));
    expect(await screen.findByRole("alert")).toHaveTextContent("network unreachable");
  });
});
