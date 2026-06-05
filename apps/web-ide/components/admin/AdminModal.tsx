// AdminModal.tsx — Dialog modale standardizzato per le pagine admin.
//
// Wrappa ModalPortal (componente esistente, vedi components/modal-portal.tsx)
// con il backdrop + il contenitore dialog coerenti col tema. Sostituisce le
// modali inline che diverse pagine admin (users, kb, sudo-manager, layout)
// duplicavano con position:fixed e stili copia-incollati (regola L / ADR 0026).
//
// Click sul backdrop chiude la modale; click sul contenuto non si propaga.
// Accessibilita': role=dialog, ESC per chiudere, focus trap delegato al chiamante.
"use client";

import { useEffect, type ReactNode } from "react";

import { useThemeColors } from "../../lib/theme";
import { ModalPortal } from "../modal-portal";

export interface AdminModalProps {
  open: boolean;
  onClose: () => void;
  title?: string;
  /** Larghezza massima del dialog (default 480px, tipico form admin). */
  maxWidth?: number;
  children: ReactNode;
}

export function AdminModal({
  open,
  onClose,
  title,
  maxWidth = 480,
  children,
}: AdminModalProps) {
  const tc = useThemeColors();

  // Chiude su ESC (UX coerente con le altre modali del progetto).
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;

  return (
    <ModalPortal>
      <div
        role="presentation"
        onClick={onClose}
        style={{
          position: "fixed",
          inset: 0,
          background: "rgba(0,0,0,0.5)",
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          zIndex: 1000,
        }}
      >
        <div
          role="dialog"
          aria-modal="true"
          aria-label={title}
          onClick={(e) => e.stopPropagation()}
          style={{
            background: tc.bg,
            color: tc.text,
            border: `1px solid ${tc.border}`,
            borderRadius: 12,
            padding: 24,
            minWidth: 320,
            maxWidth,
            width: "90%",
            maxHeight: "90vh",
            overflow: "auto",
            boxShadow: "0 10px 40px rgba(0,0,0,0.3)",
          }}
        >
          {title ? (
            <h3 style={{ fontSize: 16, fontWeight: 700, margin: "0 0 16px" }}>
              {title}
            </h3>
          ) : null}
          {children}
        </div>
      </div>
    </ModalPortal>
  );
}
