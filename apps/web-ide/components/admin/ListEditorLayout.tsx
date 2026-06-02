// ListEditorLayout.tsx — Layout condiviso per le pagine admin di tipo
// "lista + editor" (Fase G). Container con padding/maxWidth standard, slot per
// header, toolbar opzionale (es. form di aggiunta) e corpo (la lista).
"use client";

import type { ReactNode } from "react";

export interface ListEditorLayoutProps {
  header: ReactNode;
  toolbar?: ReactNode;
  children: ReactNode;
  maxWidth?: number;
}

export function ListEditorLayout({
  header,
  toolbar,
  children,
  maxWidth = 800,
}: ListEditorLayoutProps) {
  return (
    <div style={{ padding: 32, maxWidth }}>
      {header}
      {toolbar ? <div style={{ marginBottom: 20 }}>{toolbar}</div> : null}
      {children}
    </div>
  );
}
