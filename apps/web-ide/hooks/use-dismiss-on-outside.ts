"use client";

import { useEffect, type RefObject } from "react";

type ElementRef = RefObject<HTMLElement | null>;

/**
 * Chiude un elemento a comparsa (popover, menu, dropdown) quando l'utente clicca
 * fuori dalle sue zone o preme Escape.
 *
 * Punto unico (regola L / ADR 0026): la stessa coppia di listener era ripetuta
 * identica in top-bar.tsx, user-header.tsx, run-notifications.tsx,
 * project-explorer.tsx e AdminModal.tsx. I call site storici non sono ancora
 * stati fatti convergere qui: vedi il task di consolidamento.
 *
 * Accetta piu' ref perche' un pannello reso via ModalPortal NON e' discendente
 * del suo trigger nel DOM: controllando il solo contenitore, ogni clic dentro il
 * pannello risulterebbe "fuori" e lo chiuderebbe all'istante. Le zone sono in OR:
 * il clic e' "fuori" solo se cade fuori da tutte.
 *
 * I listener sono registrati solo quando `open` e' true: a pannello chiuso non
 * c'e' nessun handler globale attivo.
 */
export function useDismissOnOutside(
  open: boolean,
  zones: ElementRef | ElementRef[],
  onDismiss: () => void,
): void {
  useEffect(() => {
    if (!open) return;
    const lista = Array.isArray(zones) ? zones : [zones];
    const onPointerDown = (event: MouseEvent) => {
      const target = event.target as Node;
      const dentro = lista.some((ref) => ref.current?.contains(target));
      if (!dentro) onDismiss();
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onDismiss();
    };
    document.addEventListener("mousedown", onPointerDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("mousedown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown);
    };
    // `zones` e' un array ricreato a ogni render dal chiamante: dipendere dalla
    // sua identita' rimonterebbe i listener di continuo. Le ref sono stabili e
    // vengono lette dentro l'handler, quindi bastano `open` e `onDismiss`.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, onDismiss]);
}
