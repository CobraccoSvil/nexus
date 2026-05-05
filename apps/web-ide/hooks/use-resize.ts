"use client";

import { useCallback, useRef } from "react";

interface ResizeOptions {
  onDelta: (delta: number) => void;
  direction: "horizontal" | "vertical";
  min?: number;
}

export function useResize({ onDelta, direction, min = 80 }: ResizeOptions) {
  const startPos = useRef(0);
  const dragging = useRef(false);
  void min;

  const startDrag = useCallback(
    (initialPos: number) => {
      dragging.current = true;
      startPos.current = initialPos;

      const onMove = (ev: MouseEvent) => {
        if (!dragging.current) return;
        const pos = direction === "horizontal" ? ev.clientX : ev.clientY;
        const delta = pos - startPos.current;
        startPos.current = pos;
        onDelta(delta);
      };

      const onTouchMove = (ev: TouchEvent) => {
        if (!dragging.current || !ev.touches[0]) return;
        ev.preventDefault();
        const pos = direction === "horizontal" ? ev.touches[0].clientX : ev.touches[0].clientY;
        const delta = pos - startPos.current;
        startPos.current = pos;
        onDelta(delta);
      };

      const onUp = () => {
        dragging.current = false;
        window.removeEventListener("mousemove", onMove);
        window.removeEventListener("mouseup", onUp);
        window.removeEventListener("touchmove", onTouchMove);
        window.removeEventListener("touchend", onUp);
        document.body.style.cursor = "";
        document.body.style.userSelect = "";
      };

      document.body.style.cursor = direction === "horizontal" ? "col-resize" : "row-resize";
      document.body.style.userSelect = "none";
      window.addEventListener("mousemove", onMove);
      window.addEventListener("mouseup", onUp);
      window.addEventListener("touchmove", onTouchMove, { passive: false });
      window.addEventListener("touchend", onUp);
    },
    [onDelta, direction],
  );

  const onMouseDown = useCallback(
    (e: React.MouseEvent) => {
      e.preventDefault();
      startDrag(direction === "horizontal" ? e.clientX : e.clientY);
    },
    [startDrag, direction],
  );

  const onTouchStart = useCallback(
    (e: React.TouchEvent) => {
      if (!e.touches[0]) return;
      startDrag(direction === "horizontal" ? e.touches[0].clientX : e.touches[0].clientY);
    },
    [startDrag, direction],
  );

  return { onMouseDown, onTouchStart };
}
