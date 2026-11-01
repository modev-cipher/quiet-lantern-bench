import { useLayoutEffect, useRef, useState } from "react";

type Rect = { left: number; top: number; width: number; height: number };

const ACTIVE =
  '[aria-checked="true"], [aria-pressed="true"], [aria-selected="true"], [aria-current="page"], [aria-current="true"]';

export function useGlide(
  container: React.RefObject<HTMLElement | null>,
): Rect | null {
  const [rect, setRect] = useState<Rect | null>(null);

  useLayoutEffect(() => {
    const root = container.current;
    if (root == null) return;
    const measure = () => {
      const active = root.querySelector<HTMLElement>(ACTIVE);
      if (active == null) {
        setRect(null);
        return;
      }
      const next = {
        left: active.offsetLeft,
        top: active.offsetTop,
        width: active.offsetWidth,
        height: active.offsetHeight,
      };
      setRect((current) =>
        current != null &&
        current.left === next.left &&
        current.top === next.top &&
        current.width === next.width &&
        current.height === next.height
          ? current
          : next,
      );
    };
    measure();
    const mutations = new MutationObserver(measure);
    mutations.observe(root, {
      subtree: true,
      attributes: true,
      attributeFilter: [
        "aria-checked",
        "aria-pressed",
        "aria-selected",
        "aria-current",
      ],
      childList: true,
      characterData: true,
    });
    const documentChanges = new MutationObserver(measure);
    documentChanges.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["dir", "lang"],
    });
    const resizes = new ResizeObserver(measure);
    resizes.observe(root);
    for (const child of Array.from(root.children)) resizes.observe(child);
    window.addEventListener("resize", measure);
    return () => {
      mutations.disconnect();
      documentChanges.disconnect();
      resizes.disconnect();
      window.removeEventListener("resize", measure);
    };
  }, [container]);

  return rect;
}

function GlideThumb({
  rect,
  className,
}: {
  rect: Rect | null;
  className: string;
}) {
  return (
    <span
      aria-hidden="true"
      data-testid="glide-thumb"
      className={`glide-thumb pointer-events-none absolute left-0 top-0 ${className} ${
        rect == null ? "opacity-0" : "opacity-100"
      }`}
      style={
        rect == null
          ? undefined
          : {
              width: rect.width,
              height: rect.height,
              transform: `translate(${rect.left}px, ${rect.top}px)`,
            }
      }
    />
  );
}

export function GlideGroup({
  className,
  thumbClassName,
  children,
  ...rest
}: React.HTMLAttributes<HTMLDivElement> & { thumbClassName: string }) {
  const ref = useRef<HTMLDivElement>(null);
  const rect = useGlide(ref);
  return (
    <div ref={ref} className={`relative ${className ?? ""}`} {...rest}>
      <GlideThumb rect={rect} className={thumbClassName} />
      {children}
    </div>
  );
}

export default GlideThumb;
