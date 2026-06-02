// AdminPageHeader.tsx — Intestazione condivisa delle pagine admin (Fase G).
// Titolo + descrizione coerenti col tema, per evitare di ripetere lo stesso
// markup h2/p in ogni pagina di amministrazione.
"use client";

import { useThemeColors } from "../../lib/theme";

export interface AdminPageHeaderProps {
  title: string;
  description?: string;
}

export function AdminPageHeader({ title, description }: AdminPageHeaderProps) {
  const tc = useThemeColors();
  return (
    <div>
      <h2 style={{ fontSize: 20, fontWeight: 700, margin: "0 0 6px", color: tc.text }}>
        {title}
      </h2>
      {description ? (
        <p style={{ fontSize: 13, color: tc.textMuted, margin: "0 0 24px" }}>{description}</p>
      ) : null}
    </div>
  );
}
