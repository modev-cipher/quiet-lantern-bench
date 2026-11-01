// @vitest-environment jsdom
import { createElement, useRef, type ReactNode, StrictMode } from "react";
import { cleanup, fireEvent, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useDialogFocus } from "./useDialogFocus";

function Surface({
  id = "dialog",
  open = true,
  modal = true,
  onEscape,
  children,
}: {
  id?: string;
  open?: boolean;
  modal?: boolean;
  onEscape?: () => void;
  children?: ReactNode;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useDialogFocus(ref, open, { onEscape });
  if (!open) return null;
  return createElement(
    "div",
    { "data-modal-layer": "", "data-testid": `${id}-layer` },
    createElement(
      "div",
      {
        ref,
        role: modal ? "dialog" : "region",
        "aria-modal": modal ? "true" : undefined,
        tabIndex: -1,
        "data-testid": id,
      },
      createElement("button", { "data-testid": `${id}-first` }, "First"),
      createElement("button", { hidden: true }, "Hidden"),
      createElement("button", { disabled: true }, "Disabled"),
      createElement(
        "select",
        { tabIndex: -1 },
        createElement("option", {}, "Hidden native choice"),
      ),
      createElement("button", { "data-testid": `${id}-last` }, "Last"),
    ),
    children,
  );
}

beforeEach(() => {
  document.body.replaceChildren();
  vi.spyOn(HTMLElement.prototype, "getClientRects").mockImplementation(
    function (this: HTMLElement) {
      return (this.closest("[hidden]")
        ? []
        : [new DOMRect(0, 0, 100, 20)]) as unknown as DOMRectList;
    },
  );
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function opener() {
  const button = document.createElement("button");
  document.body.append(button);
  button.focus();
  return button;
}

describe("modal focus ownership", () => {
  it("makes background branches inert and restores their previous state", () => {
    const trigger = opener();
    const preserved = document.createElement("div");
    preserved.setAttribute("inert", "original");
    document.body.append(preserved);
    const view = render(createElement(Surface));
    expect(trigger.hasAttribute("inert")).toBe(true);
    expect(document.activeElement).toBe(view.getByTestId("dialog"));
    view.unmount();
    expect(trigger.hasAttribute("inert")).toBe(false);
    expect(preserved.getAttribute("inert")).toBe("original");
    expect(document.activeElement).toBe(trigger);
  });

  it("wraps reverse tab from the initial container and skips unavailable controls", () => {
    opener();
    const view = render(createElement(Surface));
    fireEvent.keyDown(document, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(view.getByTestId("dialog-last"));
    fireEvent.keyDown(document, { key: "Tab" });
    expect(document.activeElement).toBe(view.getByTestId("dialog-first"));
  });

  it("contains WebKitGTK reverse tab when its key name is Unidentified", () => {
    const view = render(createElement(Surface));
    fireEvent.keyDown(document, {
      key: "Unidentified",
      code: "Tab",
      shiftKey: true,
    });
    expect(document.activeElement).toBe(view.getByTestId("dialog-last"));
  });

  it("does not turn a remapped character key into Tab navigation", () => {
    const view = render(createElement(Surface));
    fireEvent.keyDown(document, { key: "x", code: "Tab" });
    expect(document.activeElement).toBe(view.getByTestId("dialog"));
  });

  it("keeps a nonmodal tutor region interactive alongside the application", () => {
    const trigger = opener();
    const view = render(createElement(Surface, { modal: false }));
    expect(trigger.hasAttribute("inert")).toBe(false);
    trigger.focus();
    fireEvent.keyDown(document, { key: "Tab" });
    expect(document.activeElement).toBe(trigger);
    view.unmount();
  });

  it("gives only the nested layer focus and Escape ownership", () => {
    const outerEscape = vi.fn();
    const innerEscape = vi.fn();
    const trigger = opener();
    const view = render(
      createElement(
        Surface,
        { id: "outer", onEscape: outerEscape },
        createElement(Surface, { id: "inner", onEscape: innerEscape }),
      ),
    );
    expect(document.activeElement).toBe(view.getByTestId("inner"));
    expect(view.getByTestId("outer").hasAttribute("inert")).toBe(true);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(innerEscape).toHaveBeenCalledOnce();
    expect(outerEscape).not.toHaveBeenCalled();
    view.unmount();
    expect(trigger.hasAttribute("inert")).toBe(false);
    expect(document.activeElement).toBe(trigger);
  });

  it("uses the latest Escape action without taking focus away from an edited control", () => {
    const previous = vi.fn();
    const next = vi.fn();
    const view = render(createElement(Surface, { onEscape: previous }));
    const last = view.getByTestId("dialog-last");
    last.focus();
    view.rerender(createElement(Surface, { onEscape: next }));
    expect(document.activeElement).toBe(last);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(previous).not.toHaveBeenCalled();
    expect(next).toHaveBeenCalledOnce();
  });

  it("blocks new background siblings and releases them after close", async () => {
    const view = render(createElement(Surface));
    const added = document.createElement("button");
    document.body.append(added);
    await waitFor(() => expect(added.hasAttribute("inert")).toBe(true));
    view.unmount();
    expect(added.hasAttribute("inert")).toBe(false);
  });

  it("keeps a dialog with no available controls focused", () => {
    const view = render(createElement(Surface));
    for (const button of view
      .getByTestId("dialog")
      .querySelectorAll("button")) {
      button.disabled = true;
    }
    fireEvent.keyDown(document, { key: "Tab" });
    expect(document.activeElement).toBe(view.getByTestId("dialog"));
  });

  it("lets an inner popup consume Escape before the modal", () => {
    const onEscape = vi.fn();
    const view = render(createElement(Surface, { onEscape }));
    const popupEscape = (event: KeyboardEvent) => event.stopPropagation();
    document.addEventListener("keydown", popupEscape);
    fireEvent.keyDown(view.getByTestId("dialog-first"), { key: "Escape" });
    document.removeEventListener("keydown", popupEscape);
    expect(onEscape).not.toHaveBeenCalled();
    fireEvent.keyDown(view.getByTestId("dialog-first"), { key: "Escape" });
    expect(onEscape).toHaveBeenCalledOnce();
  });

  it("does not dismiss the modal while an input method is composing", () => {
    const onEscape = vi.fn();
    const view = render(createElement(Surface, { onEscape }));
    fireEvent.keyDown(view.getByTestId("dialog-first"), {
      key: "Escape",
      isComposing: true,
    });
    expect(onEscape).not.toHaveBeenCalled();
  });

  it("keeps the remaining modal active when its ancestor closes first", () => {
    const trigger = opener();
    const tree = (outerOpen: boolean, innerOpen: boolean) =>
      createElement(
        "div",
        {},
        createElement(Surface, { id: "outer", open: outerOpen }),
        createElement(Surface, { id: "inner", open: innerOpen }),
      );
    const view = render(tree(true, true));
    view.rerender(tree(false, true));
    expect(trigger.hasAttribute("inert")).toBe(true);
    fireEvent.keyDown(document, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(view.getByTestId("inner-last"));
    view.rerender(tree(false, false));
    expect(trigger.hasAttribute("inert")).toBe(false);
    expect(document.activeElement).toBe(trigger);
  });

  it("releases nested ownership and listeners through Strict Mode cleanup", () => {
    const trigger = opener();
    const view = render(createElement(StrictMode, {}, createElement(Surface)));
    expect(trigger.hasAttribute("inert")).toBe(true);
    view.unmount();
    expect(trigger.hasAttribute("inert")).toBe(false);
    trigger.focus();
    fireEvent.keyDown(document, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(trigger);
  });
});
