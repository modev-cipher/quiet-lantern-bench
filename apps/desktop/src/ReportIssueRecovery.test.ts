// @vitest-environment jsdom
import { createElement } from "react";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import ReportIssueModal from "./ReportIssueModal";
import { BUG_REPORT_URL, REPORT_ISSUE_OPTIONS } from "./reportIssueModel";

const clipboardDescriptor = Object.getOwnPropertyDescriptor(
  navigator,
  "clipboard",
);
const copyDescriptor = Object.getOwnPropertyDescriptor(document, "execCommand");
const writeText = vi.fn<(text: string) => Promise<void>>();
const legacyCopy = vi.fn<(command: string) => boolean>();

beforeEach(() => {
  writeText.mockReset().mockResolvedValue(undefined);
  legacyCopy.mockReset().mockReturnValue(false);
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { writeText },
  });
  Object.defineProperty(document, "execCommand", {
    configurable: true,
    value: legacyCopy,
  });
});

afterEach(() => {
  cleanup();
  if (clipboardDescriptor)
    Object.defineProperty(navigator, "clipboard", clipboardDescriptor);
  else Reflect.deleteProperty(navigator, "clipboard");
  if (copyDescriptor)
    Object.defineProperty(document, "execCommand", copyDescriptor);
  else Reflect.deleteProperty(document, "execCommand");
  vi.restoreAllMocks();
});

function setup(
  onOpenUrl = vi
    .fn<(url: string) => Promise<void>>()
    .mockRejectedValue(new Error("no browser")),
) {
  const onClose = vi.fn();
  const props = { open: true, onClose, onOpenUrl };
  const view = render(createElement(ReportIssueModal, props));
  return { ...view, props, onClose, onOpenUrl };
}

async function failOpen() {
  fireEvent.click(screen.getByTestId("issue-option-bug"));
  await screen.findByRole("alert");
}

describe("Help link failure and recovery", () => {
  it("focuses a repeated failure after retrying the same destination", async () => {
    setup();
    await failOpen();
    const message = screen.getByRole("alert");
    await waitFor(() => expect(document.activeElement).toBe(message));
    const retry = screen.getByRole("button", { name: "Retry" });
    retry.focus();
    expect(document.activeElement).toBe(retry);
    fireEvent.click(retry);
    await waitFor(() => expect(screen.queryByRole("status")).toBeNull());
    expect(document.activeElement).toBe(message);
  });

  it("does not close a later view when an unmounted dialog's opener finishes", async () => {
    let finish = () => {};
    const opener = vi.fn<(url: string) => Promise<void>>().mockImplementation(
      () =>
        new Promise<void>((resolve) => {
          finish = resolve;
        }),
    );
    const view = setup(opener);
    fireEvent.click(screen.getByTestId("issue-option-bug"));
    view.unmount();
    await act(async () => {
      finish();
    });
    expect(view.onClose).not.toHaveBeenCalled();
  });
  it.each(REPORT_ISSUE_OPTIONS)(
    "opens the $id destination through the rendered action",
    async ({ id, url }) => {
      const opener = vi
        .fn<(url: string) => Promise<void>>()
        .mockResolvedValue(undefined);
      const { onClose } = setup(opener);
      fireEvent.click(screen.getByTestId(`issue-option-${id}`));
      await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
      expect(opener).toHaveBeenCalledWith(url);
    },
  );

  it("retains the failed destination and retries it before closing", async () => {
    const opener = vi
      .fn<(url: string) => Promise<void>>()
      .mockRejectedValueOnce(new Error("no browser"))
      .mockResolvedValueOnce(undefined);
    const { onClose } = setup(opener);
    await failOpen();
    expect(onClose).not.toHaveBeenCalled();
    const link = screen.getByRole("textbox", {
      name: "Link",
    }) as HTMLInputElement;
    expect(link.value).toBe(BUG_REPORT_URL);
    expect(link.readOnly).toBe(true);
    expect(link.dir).toBe("ltr");
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
    expect(opener.mock.calls).toEqual([[BUG_REPORT_URL], [BUG_REPORT_URL]]);
  });

  it("closes only after a successful opener response", async () => {
    let finish: (() => void) | undefined;
    const opener = vi.fn<(url: string) => Promise<void>>().mockImplementation(
      () =>
        new Promise<void>((resolve) => {
          finish = resolve;
        }),
    );
    const { onClose } = setup(opener);
    fireEvent.click(screen.getByTestId("issue-option-bug"));
    fireEvent.click(screen.getByTestId("issue-option-bug"));
    expect(opener).toHaveBeenCalledTimes(1);
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByRole("status").textContent).toContain(
      "Opening your browser",
    );
    finish?.();
    await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });

  it("ignores a pending success after closing and reopening Help", async () => {
    let finish: (() => void) | undefined;
    const opener = vi
      .fn<(url: string) => Promise<void>>()
      .mockImplementationOnce(
        () =>
          new Promise<void>((resolve) => {
            finish = resolve;
          }),
      )
      .mockResolvedValue(undefined);
    const view = setup(opener);
    fireEvent.click(screen.getByTestId("issue-option-bug"));
    view.rerender(
      createElement(ReportIssueModal, { ...view.props, open: false }),
    );
    view.rerender(createElement(ReportIssueModal, view.props));
    await act(async () => {
      finish?.();
    });
    expect(screen.queryByRole("status")).toBeNull();
    expect(view.onClose).not.toHaveBeenCalled();
    fireEvent.click(screen.getByTestId("issue-option-bug"));
    await waitFor(() => expect(view.onClose).toHaveBeenCalledTimes(1));
  });

  it("allows Escape during opening and ignores its late rejection", async () => {
    let reject: ((error: Error) => void) | undefined;
    const opener = vi.fn<(url: string) => Promise<void>>().mockImplementation(
      () =>
        new Promise<void>((_, fail) => {
          reject = fail;
        }),
    );
    const view = setup(opener);
    fireEvent.click(screen.getByTestId("issue-option-bug"));
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape" });
    expect(view.onClose).toHaveBeenCalledTimes(1);
    view.rerender(
      createElement(ReportIssueModal, { ...view.props, open: false }),
    );
    view.rerender(createElement(ReportIssueModal, view.props));
    await act(async () => {
      reject?.(new Error("late browser failure"));
    });
    expect(screen.queryByRole("status")).toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
    expect(view.onClose).toHaveBeenCalledTimes(1);
  });

  it("copies the exact destination and keeps the recovery dialog open", async () => {
    const { onClose } = setup();
    await failOpen();
    fireEvent.click(screen.getByRole("button", { name: "Copy link" }));
    await waitFor(() =>
      expect(screen.getByRole("status").textContent).toBe("Link copied."),
    );
    expect(writeText).toHaveBeenCalledWith(BUG_REPORT_URL);
    expect(onClose).not.toHaveBeenCalled();
  });

  it("can copy the visible selected link when the Clipboard API is unavailable", async () => {
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: undefined,
    });
    legacyCopy.mockReturnValue(true);
    setup();
    await failOpen();
    const copy = screen.getByRole("button", { name: "Copy link" });
    copy.focus();
    fireEvent.click(copy);
    await waitFor(() =>
      expect(screen.getByRole("status").textContent).toBe("Link copied."),
    );
    expect(legacyCopy).toHaveBeenCalledWith("copy");
    expect(document.activeElement).toBe(copy);
  });

  it.each(["refused", "threw"])(
    "leaves the link selected when clipboard access fails and fallback %s",
    async (failure) => {
      writeText.mockRejectedValue(new Error("clipboard denied"));
      if (failure === "threw")
        legacyCopy.mockImplementation(() => {
          throw new Error("copy unavailable");
        });
      setup();
      await failOpen();
      fireEvent.click(screen.getByRole("button", { name: "Copy link" }));
      await screen.findByText(
        "Couldn’t copy the link. You can select and copy it manually.",
      );
      const link = screen.getByRole("textbox", {
        name: "Link",
      }) as HTMLInputElement;
      expect(document.activeElement).toBe(link);
      expect(link.selectionStart).toBe(0);
      expect(link.selectionEnd).toBe(BUG_REPORT_URL.length);
    },
  );

  it.each(["resolved", "rejected"])(
    "ignores stale clipboard completion that %s after reopening",
    async (outcome) => {
      let finish: (() => void) | undefined;
      let fail: ((error: Error) => void) | undefined;
      writeText.mockImplementation(
        () =>
          new Promise<void>((resolve, reject) => {
            finish = resolve;
            fail = reject;
          }),
      );
      const view = setup();
      await failOpen();
      fireEvent.click(screen.getByRole("button", { name: "Copy link" }));
      view.rerender(
        createElement(ReportIssueModal, { ...view.props, open: false }),
      );
      view.rerender(createElement(ReportIssueModal, view.props));
      await act(async () => {
        if (outcome === "resolved") finish?.();
        else fail?.(new Error("late clipboard denial"));
      });
      expect(screen.queryByRole("status")).toBeNull();
      expect(screen.queryByRole("alert")).toBeNull();
      expect(screen.queryByRole("textbox")).toBeNull();
      expect(view.onClose).not.toHaveBeenCalled();
      expect(legacyCopy).not.toHaveBeenCalled();
    },
  );
});
