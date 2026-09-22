import { render, screen, waitFor } from "@testing-library/preact";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";

import { setIpc } from "../ipc/commands";
import { createMockIpc, type MockIpc } from "../ipc/mock";
import { ModelDownloads, formatBytes } from "./ModelDownloads";

let restore: (() => void) | undefined;

function mount(mock: MockIpc, onMissingChange?: (n: number) => void) {
  restore = setIpc(mock);
  return render(
    onMissingChange ? (
      <ModelDownloads announce={() => {}} onMissingChange={onMissingChange} />
    ) : (
      <ModelDownloads announce={() => {}} />
    ),
  );
}

afterEach(() => {
  restore?.();
  restore = undefined;
});

describe("formatBytes", () => {
  it("scales units and keeps large sizes readable", () => {
    expect(formatBytes(357)).toBe("357 B");
    expect(formatBytes(20_123)).toBe("20 KB");
    expect(formatBytes(78_580_914)).toBe("78.6 MB");
    // Past 100 MB the decimal is noise; only the magnitude matters.
    expect(formatBytes(377_859_675)).toBe("378 MB");
  });
});

describe("ModelDownloads", () => {
  it("lists each group with its size and install state", async () => {
    mount(createMockIpc());
    expect(await screen.findByText("Speech recognition")).toBeInTheDocument();
    expect(screen.getByText("Voice")).toBeInTheDocument();
    expect(screen.getAllByText("Not installed")).toHaveLength(2);
  });

  it("downloads everything missing and marks it installed", async () => {
    const user = userEvent.setup();
    const mock = createMockIpc();
    mount(mock);
    await user.click(await screen.findByRole("button", { name: /^Download / }));
    await waitFor(() => expect(screen.getAllByText("Installed")).toHaveLength(2));
    expect(mock.calls.some((c) => c.name === "downloadModels")).toBe(true);
  });

  it("reports how many groups are still missing", async () => {
    const seen: number[] = [];
    const user = userEvent.setup();
    mount(createMockIpc(), (n) => seen.push(n));
    await waitFor(() => expect(seen[seen.length - 1]).toBe(2));
    await user.click(await screen.findByRole("button", { name: /^Download / }));
    await waitFor(() => expect(seen[seen.length - 1]).toBe(0));
  });

  it("says where to put files someone already has", async () => {
    // A dead end with no instructions is exactly what this panel exists to
    // prevent, so the install path has to be on screen.
    mount(createMockIpc());
    expect(
      await screen.findByText("/home/you/.local/share/com.offline.practice/models"),
    ).toBeInTheDocument();
  });

  it("hides the download button once nothing is missing", async () => {
    mount(
      createMockIpc({
        catalog: [
          { id: "asr", label: "Speech recognition", detail: "d", bytes: 100, installed: true },
        ],
      }),
    );
    expect(await screen.findByText("Installed")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /^Download / })).toBeNull();
  });

  it("surfaces a failure instead of silently doing nothing", async () => {
    const user = userEvent.setup();
    mount(createMockIpc({ fail: { downloadModels: new Error("network unreachable") } }));
    await user.click(await screen.findByRole("button", { name: /^Download / }));
    expect(await screen.findByRole("alert")).toHaveTextContent("network unreachable");
  });

  it("treats a cancel as a cancel, not as an error", async () => {
    const user = userEvent.setup();
    mount(createMockIpc({ fail: { downloadModels: new Error("download cancelled") } }));
    await user.click(await screen.findByRole("button", { name: /^Download / }));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: /^Download / })).toBeInTheDocument(),
    );
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("does not offer Pause or Cancel while idle", async () => {
    mount(createMockIpc());
    await screen.findByRole("button", { name: /^Download / });
    expect(screen.queryByRole("button", { name: "Pause" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Cancel" })).toBeNull();
  });
});
