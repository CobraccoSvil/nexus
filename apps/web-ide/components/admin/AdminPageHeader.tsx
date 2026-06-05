// AdminPageHeader.tsx — Intestazione condivisa delle pagine admin.
// Titolo + descrizione + slot azione opzionale (es. bottone Refresh/Aggiungi),
// per evitare di ripetere lo stesso markup h1/h2/p in ogni pagina admin
// (regola L / ADR 0026). Lo slot ``action`` permette di adottare il componente
// anche quando il vecchio header aveva un bottone affiancato al titolo, senza
// duplicare l'header in due varianti.
"use client";

import type { ReactNode } from "react";

import { useThemeColors } from "../../lib/theme";

export interface AdminPageHeaderProps {
  title: string;
  description?: string;
  /** Contenuto opzionale (es. bottone) renderizzato a destra del titolo. */
  action?: ReactNode;
}

export function AdminPageHeader({ title, description, action }: AdminPageHeaderProps) {
  const tc = useThemeColors();
  return (
    <div
      style={{
        display: "flex",
        alignItems: "flex-start",
        justifyContent: "space-between",
        gap: 16,
        marginBottom: description ? 0 : 24,
      }}
    >
      <div style={{ minWidth: 0, flex: 1 }}>
        <h2 style={{ fontSize: 20, fontWeight: 700, margin: "0 0 6px", color: tc.text }}>
          {title}
        </h2>
        {description ? (
          <p style={{ fontSize: 13, color: tc.textMuted, margin: "0 0 24px" }}>
            {description}
          </p>
        ) : null}
      </div>
      {action ? <div style={{ flexShrink: 0 }}>{action}</div> : null}
    </div>
  );
}
