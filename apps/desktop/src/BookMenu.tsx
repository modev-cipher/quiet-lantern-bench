import {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";

import { MoreIcon } from "./Icons";
import { menuPlacement } from "./menuPlacement";

export interface BookMenuItem {
  id: string;
  label: string;
  icon?: React.ReactNode;
  testId?: string;
  tone?: "danger";
  onSelect: () => void;
}

const TABBABLE =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

function focusBeside(anchor: HTMLElement, backwards: boolean): void {
  const order = Array.from(
    document.querySelectorAll<HTMLElement>(TABBABLE),
  ).filter(
    (element) =>
      element.tabIndex >= 0 &&
      element.getClientRects().length > 0 &&
      element.closest("[inert], [hidden]") == null,
  );
  const at = order.indexOf(anchor);
  const target = at < 0 ? anchor : order[at + (backwards ? -1 : 1)];
  (target ?? anchor).focus();
}

const TRIGGER_CLASS =
  "flex h-7 shrink-0 items-center rounded-lg border border-ink-800 px-2 text-ink-400 transition-colors duration-200 hover:border-ink-700 hover:bg-ink-800/60 hover:text-ink-200 active:scale-[0.97]";

export function BookMenu({
  testId,
  items,
  label = "More actions",
  triggerClassName = TRIGGER_CLASS,
  onOpenChange,
  details,
}: {
  testId: string;
  items: BookMenuItem[];
  label?: string;
  triggerClassName?: string;
  onOpenChange?: (open: boolean) => void;
  details?: string;
}) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const itemRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const containerRef = useRef<HTMLDivElement | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);
  const [place, setPlace] = useState<{
    top: number;
    left: number;
    side: "below" | "above";
  } | null>(null);
  const menuId = useId();
  const onOpenChangeRef = useRef(onOpenChange);
  onOpenChangeRef.current = onOpenChange;

  const close = useCallback((returnFocus: boolean) => {
    setOpen(false);
    setPlace(null);
    if (returnFocus) requestAnimationFrame(() => triggerRef.current?.focus());
  }, []);

  useEffect(() => {
    onOpenChangeRef.current?.(open);
  }, [open]);

  const measure = useCallback(() => {
    const trigger = triggerRef.current;
    const menu = menuRef.current;
    if (trigger == null || menu == null) return;
    const rect = trigger.getBoundingClientRect();
    if (rect.bottom < 0 || rect.top > window.innerHeight) {
      close(false);
      return;
    }
    setPlace(
      menuPlacement(
        rect,
        { width: menu.offsetWidth, height: menu.offsetHeight },
        { width: window.innerWidth, height: window.innerHeight },
        getComputedStyle(trigger).direction === "rtl" ? "rtl" : "ltr",
      ),
    );
  }, [close]);

  useLayoutEffect(() => {
    if (open) measure();
  }, [open, items.length, measure]);

  useEffect(() => {
    if (open) itemRefs.current[active]?.focus({ preventScroll: true });
  }, [open, active]);

  useEffect(() => {
    itemRefs.current.length = items.length;
    setActive((index) => Math.min(index, Math.max(items.length - 1, 0)));
  }, [items.length]);

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Node;
      if (
        !containerRef.current?.contains(target) &&
        !menuRef.current?.contains(target)
      ) {
        close(false);
      }
    };
    let frame = 0;
    const onViewportChange = (event: Event) => {
      if (
        event.type === "scroll" &&
        menuRef.current?.contains(event.target as Node)
      ) {
        return;
      }
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(measure);
    };
    document.addEventListener("pointerdown", onPointerDown);
    window.addEventListener("scroll", onViewportChange, true);
    window.addEventListener("resize", onViewportChange);
    return () => {
      cancelAnimationFrame(frame);
      document.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("scroll", onViewportChange, true);
      window.removeEventListener("resize", onViewportChange);
    };
  }, [open, close, measure]);

  if (items.length === 0) return null;

  const openAt = (index: number) => {
    setActive(index);
    setOpen(true);
  };

  const onTriggerKeyDown = (event: React.KeyboardEvent) => {
    if (
      event.key === "ArrowDown" ||
      event.key === "Enter" ||
      event.key === " "
    ) {
      event.preventDefault();
      openAt(0);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      openAt(items.length - 1);
    }
  };

  const onMenuKeyDown = (event: React.KeyboardEvent) => {
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        setActive((index) => (index + 1) % items.length);
        break;
      case "ArrowUp":
        event.preventDefault();
        setActive((index) => (index - 1 + items.length) % items.length);
        break;
      case "Home":
        event.preventDefault();
        setActive(0);
        break;
      case "End":
        event.preventDefault();
        setActive(items.length - 1);
        break;
      case "Escape":
        event.preventDefault();
        event.stopPropagation();
        close(true);
        break;
      case "Tab": {
        event.preventDefault();
        const trigger = triggerRef.current;
        const backwards = event.shiftKey;
        close(false);
        if (trigger != null) {
          requestAnimationFrame(() => focusBeside(trigger, backwards));
        }
        break;
      }
      default:
        break;
    }
  };

  return (
    <div ref={containerRef} className="relative shrink-0">
      <button
        ref={triggerRef}
        type="button"
        data-testid={`book-menu-${testId}`}
        aria-label={label}
        title={label}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? menuId : undefined}
        onClick={() => (open ? close(false) : openAt(0))}
        onKeyDown={onTriggerKeyDown}
        className={triggerClassName}
      >
        <MoreIcon className="h-4 w-4" />
      </button>
      {open &&
        createPortal(
          <div
            ref={menuRef}
            id={menuId}
            role="menu"
            aria-label={label}
            data-testid={`book-menu-items-${testId}`}
            data-side={place?.side}
            onKeyDown={onMenuKeyDown}
            style={{
              top: place?.top ?? 0,
              left: place?.left ?? 0,
              opacity: place == null ? 0 : undefined,
            }}
            className="book-menu-pop fixed z-[70] min-w-[13rem] overflow-hidden rounded-xl border border-ink-700 bg-ink-900 py-1 shadow-overlay"
          >
            {items.map((item, index) => (
              <button
                key={item.id}
                ref={(node) => {
                  itemRefs.current[index] = node;
                }}
                type="button"
                role="menuitem"
                tabIndex={index === active ? 0 : -1}
                data-testid={item.testId ?? `book-menu-item-${item.id}`}
                onFocus={() => setActive(index)}
                onClick={() => {
                  close(false);
                  item.onSelect();
                }}
                className={`flex w-full items-center gap-2 px-3.5 py-2.5 text-start text-secondary transition-colors hover:bg-ink-800/80 focus:bg-ink-800/80 focus:outline-none ${
                  item.tone === "danger" ? "text-wash-red-ink" : "text-ink-200"
                }`}
              >
                {item.icon}
                {item.label}
              </button>
            ))}
            {details != null && details !== "" && (
              <p
                role="note"
                data-testid={`book-menu-details-${testId}`}
                className="mt-1 border-t border-ink-800 px-3.5 pt-2 pb-1.5 font-mono text-label text-ink-500"
              >
                {details}
              </p>
            )}
          </div>,
          document.body,
        )}
    </div>
  );
}

export default BookMenu;
