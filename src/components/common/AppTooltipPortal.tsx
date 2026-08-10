import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { APP_DIALOG_PORTAL_CHANGE_EVENT } from "./AppDialogPortal";

type TooltipPlacement = "top" | "right" | "left";

interface TooltipState {
  text: string;
  rect: DOMRect;
  placement: TooltipPlacement;
  visible: boolean;
}

// `.app-tooltip` sets `overflow: visible`, which would break `truncate` on
// text labels; plain text tooltip targets use the style-free `.app-tooltip-text`.
const TOOLTIP_SELECTOR =
  ".app-icon-button[aria-label], .app-tooltip[aria-label], .app-tooltip-text[aria-label]";
const VIEWPORT_MARGIN = 8;
const TARGET_GAP = 8;

const allowsTooltipForTarget = (target: HTMLElement | null) => {
  const dialogOverlay = document.querySelector(".app-dialog-overlay");
  return !dialogOverlay || Boolean(target?.closest(".app-dialog-overlay"));
};

const getPlacement = (element: HTMLElement): TooltipPlacement => {
  if (element.classList.contains("app-tooltip-side-right")) return "right";
  if (element.classList.contains("app-tooltip-side-left")) return "left";
  return "top";
};

const findTooltipTarget = (eventTarget: EventTarget | null): HTMLElement | null => {
  if (!(eventTarget instanceof Element)) return null;
  const target = eventTarget.closest<HTMLElement>(TOOLTIP_SELECTOR);
  if (!target || !target.getAttribute("aria-label")?.trim()) return null;
  return target;
};

export function AppTooltipPortal() {
  const [tooltip, setTooltip] = useState<TooltipState | null>(null);
  const [position, setPosition] = useState<{ left: number; top: number; transform: string } | null>(
    null
  );
  const tooltipRef = useRef<HTMLDivElement | null>(null);
  const activeTargetRef = useRef<HTMLElement | null>(null);
  const pointerPressedRef = useRef(false);
  const showFrameRef = useRef<number | null>(null);

  const hideTooltip = useCallback(() => {
    activeTargetRef.current = null;
    if (showFrameRef.current !== null) {
      window.cancelAnimationFrame(showFrameRef.current);
      showFrameRef.current = null;
    }
    setTooltip((current) => (current ? { ...current, visible: false } : null));
    setPosition(null);
  }, []);

  const showTooltip = useCallback(
    (target: HTMLElement) => {
      if (!allowsTooltipForTarget(target)) {
        hideTooltip();
        return;
      }

      const text = target.getAttribute("aria-label")?.trim();
      if (!text) return;

      activeTargetRef.current = target;
      if (showFrameRef.current !== null) {
        window.cancelAnimationFrame(showFrameRef.current);
      }

      setTooltip({
        text,
        rect: target.getBoundingClientRect(),
        placement: getPlacement(target),
        visible: false,
      });
      setPosition(null);

      showFrameRef.current = window.requestAnimationFrame(() => {
        showFrameRef.current = null;
        setTooltip((current) =>
          current && activeTargetRef.current === target ? { ...current, visible: true } : current
        );
      });
    },
    [hideTooltip]
  );

  useEffect(() => {
    const handlePointerDown = () => {
      pointerPressedRef.current = true;
      hideTooltip();
    };

    const handlePointerRelease = () => {
      pointerPressedRef.current = false;
    };

    const handlePointerOver = (event: PointerEvent) => {
      if (pointerPressedRef.current || event.buttons !== 0) {
        hideTooltip();
        return;
      }
      const target = findTooltipTarget(event.target);
      if (target && target !== activeTargetRef.current) {
        showTooltip(target);
      }
    };

    const handlePointerOut = (event: PointerEvent) => {
      const activeTarget = activeTargetRef.current;
      if (!activeTarget) return;
      const relatedTarget = event.relatedTarget;
      if (relatedTarget instanceof Node && activeTarget.contains(relatedTarget)) return;
      if (event.target instanceof Node && activeTarget.contains(event.target)) {
        hideTooltip();
      }
    };

    const handleFocusIn = (event: FocusEvent) => {
      if (pointerPressedRef.current) return;
      const target = findTooltipTarget(event.target);
      if (target) showTooltip(target);
    };

    const handleFocusOut = (event: FocusEvent) => {
      const activeTarget = activeTargetRef.current;
      if (!activeTarget) return;
      if (event.target instanceof Node && activeTarget.contains(event.target)) {
        hideTooltip();
      }
    };

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") hideTooltip();
    };

    const handleViewportChange = () => {
      const activeTarget = activeTargetRef.current;
      if (!activeTarget) return;
      setTooltip((current) =>
        current
          ? {
              ...current,
              rect: activeTarget.getBoundingClientRect(),
              placement: getPlacement(activeTarget),
            }
          : current
      );
    };

    const handleDialogPortalChange = () => {
      if (!allowsTooltipForTarget(activeTargetRef.current)) {
        hideTooltip();
      }
    };

    document.addEventListener("pointerdown", handlePointerDown, true);
    document.addEventListener("pointerover", handlePointerOver);
    document.addEventListener("pointerout", handlePointerOut);
    document.addEventListener("focusin", handleFocusIn);
    document.addEventListener("focusout", handleFocusOut);
    document.addEventListener("keydown", handleKeyDown);
    document.addEventListener(APP_DIALOG_PORTAL_CHANGE_EVENT, handleDialogPortalChange);
    window.addEventListener("resize", handleViewportChange);
    window.addEventListener("scroll", handleViewportChange, true);
    window.addEventListener("pointerup", handlePointerRelease, true);
    window.addEventListener("pointercancel", handlePointerRelease, true);
    window.addEventListener("blur", handlePointerRelease);

    return () => {
      document.removeEventListener("pointerdown", handlePointerDown, true);
      document.removeEventListener("pointerover", handlePointerOver);
      document.removeEventListener("pointerout", handlePointerOut);
      document.removeEventListener("focusin", handleFocusIn);
      document.removeEventListener("focusout", handleFocusOut);
      document.removeEventListener("keydown", handleKeyDown);
      document.removeEventListener(APP_DIALOG_PORTAL_CHANGE_EVENT, handleDialogPortalChange);
      window.removeEventListener("resize", handleViewportChange);
      window.removeEventListener("scroll", handleViewportChange, true);
      window.removeEventListener("pointerup", handlePointerRelease, true);
      window.removeEventListener("pointercancel", handlePointerRelease, true);
      window.removeEventListener("blur", handlePointerRelease);
      if (showFrameRef.current !== null) {
        window.cancelAnimationFrame(showFrameRef.current);
      }
    };
  }, [hideTooltip, showTooltip]);

  useLayoutEffect(() => {
    if (!tooltip || !tooltipRef.current) return;

    const tooltipRect = tooltipRef.current.getBoundingClientRect();
    const maxLeft = Math.max(
      VIEWPORT_MARGIN,
      window.innerWidth - tooltipRect.width - VIEWPORT_MARGIN
    );
    const maxTop = Math.max(
      VIEWPORT_MARGIN,
      window.innerHeight - tooltipRect.height - VIEWPORT_MARGIN
    );
    let left = tooltip.rect.left + tooltip.rect.width / 2 - tooltipRect.width / 2;
    let top = tooltip.rect.top - tooltipRect.height - TARGET_GAP;

    if (tooltip.placement === "right") {
      left = tooltip.rect.right + TARGET_GAP;
      top = tooltip.rect.top + tooltip.rect.height / 2 - tooltipRect.height / 2;
    } else if (tooltip.placement === "left") {
      left = tooltip.rect.left - tooltipRect.width - TARGET_GAP;
      top = tooltip.rect.top + tooltip.rect.height / 2 - tooltipRect.height / 2;
    }

    if (left < VIEWPORT_MARGIN && tooltip.placement === "left") {
      left = tooltip.rect.right + TARGET_GAP;
    } else if (
      left + tooltipRect.width > window.innerWidth - VIEWPORT_MARGIN &&
      tooltip.placement === "right"
    ) {
      left = tooltip.rect.left - tooltipRect.width - TARGET_GAP;
    }

    if (top < VIEWPORT_MARGIN && tooltip.placement === "top") {
      top = tooltip.rect.bottom + TARGET_GAP;
    }

    setPosition({
      left: Math.min(Math.max(left, VIEWPORT_MARGIN), maxLeft),
      top: Math.min(Math.max(top, VIEWPORT_MARGIN), maxTop),
      transform: tooltip.visible ? "translateY(0)" : "translateY(6px)",
    });
  }, [tooltip]);

  if (!tooltip || typeof document === "undefined") return null;

  return createPortal(
    <div
      ref={tooltipRef}
      className={`app-tooltip-portal ${tooltip.visible && position ? "is-visible" : ""}`}
      style={{
        left: position?.left ?? 0,
        top: position?.top ?? 0,
        transform: position?.transform ?? "translateY(6px)",
      }}
      role="tooltip"
    >
      {tooltip.text}
    </div>,
    document.body
  );
}
