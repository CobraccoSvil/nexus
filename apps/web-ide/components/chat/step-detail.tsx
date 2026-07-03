"use client";

// Helper condivisi per il DETTAGLIO di uno step/tool agente (punto unico, regola
// L): troncamento espandibile del testo (parametri/risultato) e formattazione
// leggibile dell'input strutturato. Estratti da message-list.tsx per essere
// riusati anche dal nastro attivita' (activity-stream.tsx) senza duplicazione e
// senza creare un ciclo di import fra i due componenti.

import { useState } from "react";
import type { useThemeColors } from "../../lib/theme";

type ThemeColors = ReturnType<typeof useThemeColors>;

/** Blocco di testo troncato a `maxLen` caratteri con toggle "Mostra tutto". */
export function InlineTruncated({
  text,
  maxLen = 400,
  tc,
  mono = true,
}: {
  text: string;
  maxLen?: number;
  tc: ThemeColors;
  mono?: boolean;
}) {
  const [full, setFull] = useState(false);
  const truncated = text.length > maxLen;
  const display = full || !truncated ? text : text.slice(0, maxLen) + "...";
  return (
    <div>
      <pre
        style={{
          fontFamily: mono ? "var(--font-mono)" : "inherit",
          fontSize: 11,
          whiteSpace: "pre-wrap",
          wordBreak: "break-word",
          margin: 0,
          maxHeight: full ? 500 : 160,
          overflowY: "auto",
          color: tc.text,
          background: `${tc.bgInput ?? tc.border}40`,
          borderRadius: 4,
          padding: "4px 6px",
        }}
      >
        {display}
      </pre>
      {truncated && (
        <button
          type="button"
          onClick={() => setFull((v) => !v)}
          style={{
            fontSize: 10,
            color: tc.accent,
            background: "none",
            border: "none",
            cursor: "pointer",
            padding: "2px 0",
            fontWeight: 600,
          }}
        >
          {full ? "Comprimi" : `Mostra tutto (${text.length.toLocaleString()} car.)`}
        </button>
      )}
    </div>
  );
}

/** Formatta l'input strutturato di un tool in testo leggibile, comprimendo i
 *  valori enormi (stringhe/oggetti > 300 char) a un placeholder con la lunghezza,
 *  MAI il JSON grezzo integrale. */
export function formatStepInput(input: Record<string, unknown>): string {
  const lines: string[] = [];
  for (const [key, val] of Object.entries(input)) {
    if (typeof val === "string" && val.length > 300) {
      lines.push(`${key}: [${val.length} car.]`);
    } else if (typeof val === "object" && val !== null) {
      const j = JSON.stringify(val);
      lines.push(j.length > 300 ? `${key}: [oggetto, ${j.length} car.]` : `${key}: ${j}`);
    } else {
      lines.push(`${key}: ${String(val)}`);
    }
  }
  return lines.join("\n");
}
