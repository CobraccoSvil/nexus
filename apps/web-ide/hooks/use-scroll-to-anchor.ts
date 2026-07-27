"use client";

import { useCallback, useEffect, useRef } from "react";
import { runScopedAnchorId } from "../lib/use-chat/activity-stream";
import { withAlpha } from "../lib/color";

// Durata del flash sulla riga bersaglio, coerente col pattern timer di
// usePanelHighlight (dispatcher-status.tsx): stato/timer che spegne.
const FLASH_MS = 1200;

interface FlashSnapshot {
  el: HTMLElement;
  boxShadow: string;
  background: string;
  transition: string;
}

/**
 * Deep-link dal centro notifiche alla riga esatta del nastro attivita'.
 *
 * Punto unico (regola L) dello scroll-verso-ancora del nastro: dato il runId e
 * l'ancora locale dell'evento (+ quella del segmento come fallback), risolve
 * l'id DOM scopato per run (runScopedAnchorId), scorre l'antenato scrollabile
 * con scrollIntoView (che risale da solo il contenitore overflow) e applica un
 * flash temporaneo (~1200ms) sulla riga, poi lo spegne via timer.
 *
 * Fallback evento -> segmento: se l'evento e' stato cappato dalla vista live
 * (nessun nodo DOM) atterra sul segmento, che e' sempre presente. La condizione
 * "evento cappato" e' letta dal DOM (getElementById null), non indovinata.
 *
 * Ritorna una funzione stabile; il flash e' idempotente (un nuovo target spegne
 * il precedente) e viene ripulito allo smontaggio.
 */
export function useScrollToAnchor(): (
  runId: string,
  eventAnchor: string | undefined,
  segmentAnchor: string | undefined,
  accent: string,
) => boolean {
  const timerRef = useRef<number | null>(null);
  const snapshotRef = useRef<FlashSnapshot | null>(null);

  const clearFlash = useCallback(() => {
    if (timerRef.current != null) {
      window.clearTimeout(timerRef.current);
      timerRef.current = null;
    }
    const prev = snapshotRef.current;
    if (prev) {
      prev.el.style.boxShadow = prev.boxShadow;
      prev.el.style.background = prev.background;
      prev.el.style.transition = prev.transition;
      snapshotRef.current = null;
    }
  }, []);

  const scrollToAnchor = useCallback(
    (
      runId: string,
      eventAnchor: string | undefined,
      segmentAnchor: string | undefined,
      accent: string,
    ): boolean => {
      const byLocal = (local?: string): HTMLElement | null =>
        local ? document.getElementById(runScopedAnchorId(runId, local)) : null;
      const target = byLocal(eventAnchor) ?? byLocal(segmentAnchor);
      if (!target) return false;

      clearFlash();
      target.scrollIntoView({ block: "center", behavior: "smooth" });

      // Flash inline dal tema (accent): box-shadow + velatura, ripristinati dal
      // timer. Salviamo gli stili inline precedenti per non lasciare residui.
      snapshotRef.current = {
        el: target,
        boxShadow: target.style.boxShadow,
        background: target.style.background,
        transition: target.style.transition,
      };
      target.style.transition = "box-shadow 200ms ease, background 200ms ease";
      target.style.boxShadow = `0 0 0 2px ${accent}`;
      target.style.background = withAlpha(accent, 0.12);
      timerRef.current = window.setTimeout(clearFlash, FLASH_MS);
      return true;
    },
    [clearFlash],
  );

  useEffect(() => clearFlash, [clearFlash]);

  return scrollToAnchor;
}
