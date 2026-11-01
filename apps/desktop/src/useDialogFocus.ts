import { useEffect, useRef, type RefObject } from "react";

interface DialogFocusOptions {
  initialFocusRef?: RefObject<HTMLElement | null>;
  initialFocusSelector?: string;
  fallbackSelector?: string;
  onEscape?: () => void;
}

interface ModalEntry {
  dialog: HTMLElement;
  layer: HTMLElement;
  options: () => DialogFocusOptions;
  returnTargets: HTMLElement[];
}

const FOCUSABLE =
  'a[href], button, input:not([type="hidden"]), select, textarea, summary, [tabindex], [contenteditable="true"], audio[controls], video[controls]';

function available(element: HTMLElement): boolean {
  return (
    element.isConnected &&
    !element.closest('[hidden], [inert], [aria-hidden="true"]') &&
    !element.matches(":disabled") &&
    element.getClientRects().length > 0 &&
    getComputedStyle(element).visibility !== "hidden"
  );
}

function tabStops(dialog: HTMLElement): HTMLElement[] {
  return Array.from(dialog.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
    (element) =>
      available(element) &&
      (element.tabIndex >= 0 ||
        (element.isContentEditable && !element.hasAttribute("tabindex"))),
  );
}

class ModalFocusStack {
  private entries: ModalEntry[] = [];
  private inertBefore = new Map<HTMLElement, string | null>();
  private observer: MutationObserver;

  constructor(private document: Document) {
    this.observer = new MutationObserver((records) => {
      const layer = this.top?.layer;
      if (
        layer &&
        records.some(
          (record) =>
            record.target.contains(layer) ||
            Array.from(record.removedNodes).some((node) =>
              node.contains(layer),
            ),
        )
      ) {
        this.updateInert();
      }
    });
  }

  get top(): ModalEntry | undefined {
    for (let index = this.entries.length - 1; index >= 0; index -= 1) {
      if (this.entries[index].dialog.isConnected) return this.entries[index];
    }
    return undefined;
  }

  add(dialog: HTMLElement, options: () => DialogFocusOptions): () => void {
    const previous = this.document.activeElement;
    const entry: ModalEntry = {
      dialog,
      layer: dialog.closest<HTMLElement>("[data-modal-layer]") ?? dialog,
      options,
      returnTargets: [
        ...(previous instanceof HTMLElement ? [previous] : []),
        ...(this.top?.returnTargets ?? []),
      ],
    };
    const descendants = this.entries.findIndex((current) =>
      entry.layer.contains(current.layer),
    );
    this.entries.splice(
      descendants < 0 ? this.entries.length : descendants,
      0,
      entry,
    );
    if (this.entries.length === 1) {
      this.document.addEventListener("keydown", this.onKeyDown, true);
      this.document.defaultView?.addEventListener("keydown", this.onEscape);
      this.observer.observe(this.document.body, {
        childList: true,
        subtree: true,
      });
    }
    this.updateInert();
    if (this.top === entry) this.focusInitial(entry);
    return () => this.remove(entry);
  }

  private focusInitial(entry: ModalEntry) {
    const options = entry.options();
    const preferred =
      options.initialFocusRef?.current ??
      (options.initialFocusSelector
        ? entry.dialog.querySelector<HTMLElement>(options.initialFocusSelector)
        : null);
    (preferred && available(preferred) ? preferred : entry.dialog).focus({
      preventScroll: true,
    });
  }

  private restoreInert() {
    for (const [element, attribute] of this.inertBefore) {
      if (attribute === null) element.removeAttribute("inert");
      else element.setAttribute("inert", attribute);
    }
    this.inertBefore.clear();
  }

  private updateInert() {
    this.restoreInert();
    let branch = this.top?.layer;
    while (branch?.parentElement) {
      const parent = branch.parentElement;
      for (const sibling of parent.children) {
        if (sibling !== branch && sibling instanceof HTMLElement) {
          this.inertBefore.set(sibling, sibling.getAttribute("inert"));
          sibling.setAttribute("inert", "");
        }
      }
      if (parent === this.document.body) break;
      branch = parent;
    }
  }

  private remove(entry: ModalEntry) {
    const index = this.entries.indexOf(entry);
    if (index < 0) return;
    const wasTop = index === this.entries.length - 1;
    this.entries.splice(index, 1);
    this.updateInert();
    if (this.entries.length === 0) {
      this.observer.disconnect();
      this.document.removeEventListener("keydown", this.onKeyDown, true);
      this.document.defaultView?.removeEventListener("keydown", this.onEscape);
    }
    if (!wasTop) return;
    const fallback = entry.options().fallbackSelector;
    const targets = [
      ...entry.returnTargets,
      ...(fallback ? [this.document.querySelector<HTMLElement>(fallback)] : []),
    ];
    const target = targets.find(
      (candidate) =>
        candidate != null &&
        available(candidate) &&
        (!this.top || this.top.dialog.contains(candidate)),
    );
    if (target) target.focus({ preventScroll: true });
    else if (this.top) this.focusInitial(this.top);
  }

  private onEscape = (event: KeyboardEvent) => {
    const entry = this.top;
    if (!entry || event.defaultPrevented || event.isComposing) return;
    if (event.key === "Escape" && entry.options().onEscape) {
      event.preventDefault();
      event.stopPropagation();
      entry.options().onEscape?.();
    }
  };

  private onKeyDown = (event: KeyboardEvent) => {
    const entry = this.top;
    const tabPressed =
      event.key === "Tab" ||
      (event.key === "Unidentified" && event.code === "Tab");
    if (!entry || event.defaultPrevented || event.isComposing || !tabPressed)
      return;
    const stops = tabStops(entry.dialog);
    const position = stops.indexOf(this.document.activeElement as HTMLElement);
    if (stops.length === 0) {
      event.preventDefault();
      entry.dialog.focus({ preventScroll: true });
    } else if (
      position < 0 ||
      (event.shiftKey ? position === 0 : position === stops.length - 1)
    ) {
      event.preventDefault();
      stops[event.shiftKey ? stops.length - 1 : 0].focus();
    }
  };
}

const modalStacks = new WeakMap<Document, ModalFocusStack>();

export function isTopDialog(dialog: HTMLElement | null): boolean {
  return (
    dialog != null &&
    modalStacks.get(dialog.ownerDocument)?.top?.dialog === dialog
  );
}

export function useDialogFocus(
  dialogRef: RefObject<HTMLElement | null>,
  active: boolean = true,
  options: DialogFocusOptions = {},
) {
  const latest = useRef(options);
  latest.current = options;
  useEffect(() => {
    const element = dialogRef.current;
    if (!active || !element) return;
    const document = element.ownerDocument;
    if (element.getAttribute("aria-modal") === "true") {
      let stack = modalStacks.get(document);
      if (!stack) {
        stack = new ModalFocusStack(document);
        modalStacks.set(document, stack);
      }
      return stack.add(element, () => latest.current);
    }
    const trigger = document.activeElement;
    if (!element.closest("[inert]")) element.focus();
    return () => {
      if (trigger instanceof HTMLElement && available(trigger)) trigger.focus();
    };
  }, [dialogRef, active]);
}
