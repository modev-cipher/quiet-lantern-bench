import { useCallback, useEffect, useRef, useState } from "react";

const ITEM_SELECTOR =
  '[role="menuitemradio"]:not([disabled]), [role="menuitem"]:not([disabled])';

function menuItems(menu: HTMLElement | null): HTMLElement[] {
  return Array.from(menu?.querySelectorAll<HTMLElement>(ITEM_SELECTOR) ?? []);
}

export function usePopupMenu() {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: PointerEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const items = menuItems(menuRef.current);
    const checked = items.find(
      (item) => item.getAttribute("aria-checked") === "true",
    );
    (checked ?? items[0])?.focus();
  }, [open]);

  const close = useCallback(() => {
    setOpen(false);
    triggerRef.current?.focus();
  }, []);

  const rootProps = {
    ref: rootRef,
    onKeyDown: (event: React.KeyboardEvent) => {
      if (event.key === "Escape" && open) {
        event.preventDefault();
        event.stopPropagation();
        close();
      }
    },
  };

  const triggerProps = {
    ref: triggerRef,
    "aria-haspopup": "menu" as const,
    "aria-expanded": open,
    onClick: () => setOpen((value) => !value),
    onKeyDown: (event: React.KeyboardEvent) => {
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        setOpen(true);
      }
    },
  };

  const menuProps = {
    ref: menuRef,
    role: "menu" as const,
    onKeyDown: (event: React.KeyboardEvent) => {
      if (event.key === "Tab") {
        setOpen(false);
        return;
      }
      if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) {
        return;
      }
      event.preventDefault();
      const items = menuItems(menuRef.current);
      if (items.length === 0) return;
      const at = items.indexOf(document.activeElement as HTMLElement);
      const next =
        event.key === "Home"
          ? 0
          : event.key === "End"
            ? items.length - 1
            : event.key === "ArrowDown"
              ? (at + 1) % items.length
              : (at - 1 + items.length) % items.length;
      items[next]?.focus();
    },
  };

  return { open, close, rootProps, triggerProps, menuProps };
}
