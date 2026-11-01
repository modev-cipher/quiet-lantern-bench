// @vitest-environment jsdom
import { createElement, useState } from "react";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { CustomDropdown, type DropdownOption } from "./CustomDropdown";

const originalScrollIntoView = HTMLElement.prototype.scrollIntoView;

const OPTIONS: DropdownOption[] = [
  { value: "adam", label: "🇺🇸 Adam", group: "English" },
  { value: "bella", label: "🇺🇸 Bella", group: "English" },
  { value: "bob", label: "🇺🇸 Bob", group: "English" },
  { value: "heart", label: "🇺🇸 Heart", group: "English" },
];

function Example({
  options = OPTIONS,
  disabled = false,
}: {
  options?: DropdownOption[];
  disabled?: boolean;
}) {
  const [value, setValue] = useState("bella");
  return createElement(
    "div",
    {},
    createElement("label", { htmlFor: "voice" }, "Voice"),
    createElement(CustomDropdown, {
      id: "voice",
      label: "Voice",
      testId: "voice",
      value,
      onChange: setValue,
      options,
      disabled,
    }),
    createElement("output", { "data-testid": "selected" }, value),
    createElement("button", {}, "Next"),
  );
}

beforeEach(() => {
  HTMLElement.prototype.scrollIntoView = vi.fn();
});

afterEach(() => {
  cleanup();
  HTMLElement.prototype.scrollIntoView = originalScrollIntoView;
  vi.restoreAllMocks();
});

function combo() {
  return screen.getByRole("combobox", { name: "Voice" });
}

function activeOption() {
  return document.getElementById(
    combo().getAttribute("aria-activedescendant") ?? "",
  );
}

describe("visible selector keyboard and naming contract", () => {
  it("connects the field label and popup to the visible control", () => {
    render(createElement(Example));
    expect((screen.getByText("Voice") as HTMLLabelElement).control).toBe(
      combo(),
    );
    fireEvent.keyDown(combo(), { key: "ArrowDown" });
    expect(screen.getByRole("listbox", { name: "Voice" }).id).toBe(
      combo().getAttribute("aria-controls"),
    );
    expect(activeOption()?.textContent).toContain("Bella");
  });

  it("explores options without changing the chosen value and cancels with Escape", () => {
    render(createElement(Example));
    combo().focus();
    fireEvent.keyDown(combo(), { key: "ArrowDown" });
    fireEvent.keyDown(combo(), { key: "ArrowDown" });
    expect(activeOption()?.textContent).toContain("Bob");
    expect(screen.getByTestId("selected").textContent).toBe("bella");
    fireEvent.keyDown(combo(), { key: "Escape" });
    expect(combo().getAttribute("aria-expanded")).toBe("false");
    expect(combo().hasAttribute("aria-activedescendant")).toBe(false);
    expect(screen.getByTestId("selected").textContent).toBe("bella");
    expect(document.activeElement).toBe(combo());
  });

  it("accepts the active value with Enter, Space, and Tab", () => {
    render(createElement(Example));
    fireEvent.keyDown(combo(), { key: "End" });
    fireEvent.keyDown(combo(), { key: "Enter" });
    expect(screen.getByTestId("selected").textContent).toBe("heart");
    fireEvent.keyDown(combo(), { key: "Home" });
    fireEvent.keyDown(combo(), { key: " " });
    expect(screen.getByTestId("selected").textContent).toBe("adam");
    fireEvent.keyDown(combo(), { key: "End" });
    const tab = new KeyboardEvent("keydown", {
      key: "Tab",
      bubbles: true,
      cancelable: true,
    });
    fireEvent(combo(), tab);
    expect(tab.defaultPrevented).toBe(false);
    expect(screen.getByTestId("selected").textContent).toBe("heart");
    expect(combo().getAttribute("aria-expanded")).toBe("false");
  });

  it("uses bounded arrows and page navigation for long lists", () => {
    render(
      createElement(Example, {
        options: Array.from({ length: 35 }, (_, i) => ({
          value: String(i),
          label: `Model ${i}`,
        })),
      }),
    );
    fireEvent.keyDown(combo(), { key: "Home" });
    fireEvent.keyDown(combo(), { key: "ArrowUp" });
    expect(activeOption()?.textContent).toBe("Model 0");
    fireEvent.keyDown(combo(), { key: "PageDown" });
    expect(activeOption()?.textContent).toBe("Model 10");
    fireEvent.keyDown(combo(), { key: "End" });
    fireEvent.keyDown(combo(), { key: "ArrowDown" });
    expect(activeOption()?.textContent).toBe("Model 34");
    expect(HTMLElement.prototype.scrollIntoView).toHaveBeenCalled();
  });

  it("supports name type-ahead beyond a leading flag and cycles repeated characters", () => {
    render(createElement(Example));
    fireEvent.keyDown(combo(), { key: "b" });
    expect(activeOption()?.textContent).toContain("Bob");
    fireEvent.keyDown(combo(), { key: "b" });
    expect(activeOption()?.textContent).toContain("Bella");
    fireEvent.keyDown(combo(), { key: "e" });
    expect(activeOption()?.textContent).toContain("Bella");
    fireEvent.keyDown(combo(), { key: "Enter" });
    expect(screen.getByTestId("selected").textContent).toBe("bella");
  });

  it("keeps punctuation while searching model identifiers", () => {
    render(
      createElement(Example, {
        options: [
          { value: "six", label: "GPT-6 Luna" },
          { value: "five", label: "GPT-5 Luna" },
        ],
      }),
    );
    for (const key of "gpt-6") fireEvent.keyDown(combo(), { key });
    expect(activeOption()?.textContent).toBe("GPT-6 Luna");
  });

  it("navigates grouped options in their visible order", () => {
    render(
      createElement(Example, {
        options: [
          { value: "a", label: "Alpha", group: "First" },
          { value: "b", label: "Beta", group: "Second" },
          { value: "c", label: "Charlie", group: "First" },
        ],
      }),
    );
    fireEvent.keyDown(combo(), { key: "Home" });
    fireEvent.keyDown(combo(), { key: "ArrowDown" });
    expect(activeOption()?.textContent).toBe("Charlie");
  });

  it("supports Arabic name searches", () => {
    render(
      createElement(Example, {
        options: [
          { value: "a", label: "أحمد" },
          { value: "b", label: "باسم" },
          { value: "c", label: "بدر" },
        ],
      }),
    );
    fireEvent.keyDown(combo(), { key: "ب" });
    fireEvent.keyDown(combo(), { key: "د" });
    expect(activeOption()?.textContent).toBe("بدر");
  });

  it("starts a new type-ahead query after a pause", () => {
    vi.spyOn(Date, "now").mockReturnValueOnce(100).mockReturnValueOnce(1000);
    render(createElement(Example));
    fireEvent.keyDown(combo(), { key: "b" });
    fireEvent.keyDown(combo(), { key: "h" });
    expect(activeOption()?.textContent).toContain("Heart");
  });

  it("accepts the explored choice when focus leaves the field", () => {
    render(createElement(Example));
    fireEvent.keyDown(combo(), { key: "End" });
    fireEvent.blur(combo(), {
      relatedTarget: screen.getByRole("button", { name: "Next" }),
    });
    expect(screen.getByTestId("selected").textContent).toBe("heart");
    expect(combo().getAttribute("aria-expanded")).toBe("false");
  });

  it("does not let popup Escape reach the containing dialog", () => {
    const outer = vi.fn();
    render(createElement("div", { onKeyDown: outer }, createElement(Example)));
    fireEvent.keyDown(combo(), { key: "ArrowDown" });
    outer.mockClear();
    fireEvent.keyDown(combo(), { key: "Escape" });
    expect(outer).not.toHaveBeenCalled();
  });

  it("keeps active references valid when the catalog changes", () => {
    const view = render(createElement(Example));
    fireEvent.keyDown(combo(), { key: "End" });
    view.rerender(createElement(Example, { options: OPTIONS.slice(0, 2) }));
    expect(activeOption()?.textContent).toContain("Bella");
    expect(screen.getByTestId("selected").textContent).toBe("bella");
    view.rerender(createElement(Example, { options: [] }));
    expect(combo().hasAttribute("aria-activedescendant")).toBe(false);
    fireEvent.keyDown(combo(), { key: "Enter" });
    expect(screen.getByTestId("selected").textContent).toBe("bella");
  });

  it("does not open a disabled control", () => {
    render(createElement(Example, { disabled: true }));
    fireEvent.keyDown(combo(), { key: "ArrowDown" });
    expect(combo().getAttribute("aria-expanded")).toBe("false");
  });
});
