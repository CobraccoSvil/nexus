"use client";

import { useEffect, useRef, type ReactNode } from "react";
import { createPortal } from "react-dom";

/**
 * Renderizza i children in un portal attaccato direttamente a `<body>`.
 *
 * Serve per i dialog modali che devono coprire TUTTA l'interfaccia
 * (backdrop + z-index effettivo rispetto al viewport), indipendentemente
 * dal contesto di stacking del componente chiamante.
 *
 * Senza portal, un dialog con `position: fixed; inset: 0` renderizzato
 * dentro una cella del grid CSS dell'IDE puo' non coprire le celle
 * adiacenti (sidebar sinistra, pannello inferiore) se un antenato
 * crea un nuovo stacking context.
 */
export function ModalPortal({ children }: { children: ReactNode }) {
  const elRef = useRef<HTMLDivElement | null>(null);

  if (typeof window !== "undefined" && !elRef.current) {
    elRef.current = document.createElement("div");
  }

  useEffect(() => {
    const el = elRef.current;
    if (!el) return;
    document.body.appendChild(el);
    return () => {
      document.body.removeChild(el);
    };
  }, []);

  if (!elRef.current) return null;
  return createPortal(children, elRef.current);
}
