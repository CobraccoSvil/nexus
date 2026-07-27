"use client";

import { useState } from "react";
import type { useThemeColors } from "../lib/theme";

/**
 * Componente per testi troncati con tooltip.
 * Mostra il testo con ellipsis e un tooltip al hover.
 */
export function TruncatedText({
  text,
  className = "",
  maxWidth = 300,
  tc,
  style = {},
}: {
  text: string;
  className?: string;
  maxWidth?: number | string;
  tc?: ReturnType<typeof useThemeColors>;
  style?: React.CSSProperties;
}) {
  const [showTooltip, setShowTooltip] = useState(false);

  // Il tetto e' un MASSIMO che non puo' superare il contenitore. Un numero da
  // solo non basta: `maxWidth: 300` in una colonna da 204 sfonda comunque, ed e'
  // il difetto che questo componente causava (26 elementi fuori dalla sidebar).
  const tettoReale = typeof maxWidth === "number" ? `min(${maxWidth}px, 100%)` : maxWidth;

  // Se non c'è theme colors, fallback a title nativo
  if (!tc) {
    return (
      <span
        className={className}
        style={{
          overflow: "hidden",
          textOverflow: "ellipsis",
          whiteSpace: "nowrap",
          maxWidth: tettoReale,
          display: "inline-block",
          ...style,
        }}
        title={text}
      >
        {text}
      </span>
    );
  }

  return (
    <div
      style={{
        position: "relative",
        display: "inline-block",
        // Era `width: maxWidth`: una LARGHEZZA FISSA, non un tetto. Il componente
        // occupava 300px anche per "pnpm run dev" e sfondava ogni contenitore piu'
        // stretto — il nome dice maxWidth, il codice imponeva width.
        maxWidth: tettoReale,
        minWidth: 0,
      }}
      onMouseEnter={() => setShowTooltip(true)}
      onMouseLeave={() => setShowTooltip(false)}
    >
      <span
        className={className}
        style={{
          overflow: "hidden",
          textOverflow: "ellipsis",
          whiteSpace: "nowrap",
          // 100% del wrapper, che porta gia' il tetto: cosi' l'ellipsis scatta al
          // bordo reale invece che a un numero che il contenitore non ha.
          maxWidth: "100%",
          display: "block",
          ...style,
        }}
      >
        {text}
      </span>

      {showTooltip && text && (
        <div
          style={{
            position: "absolute",
            bottom: "100%",
            left: "50%",
            transform: "translateX(-50%)",
            marginBottom: 8,
            background: tc.bgCard,
            border: `1px solid ${tc.border}`,
            borderRadius: 6,
            padding: "6px 10px",
            fontSize: 12,
            color: tc.text,
            zIndex: 1000,
            boxShadow: "0 2px 8px rgba(0,0,0,0.2)",
            maxWidth: 400,
            wordBreak: "break-word",
            whiteSpace: "normal",
            pointerEvents: "none",
          }}
        >
          {text}
        </div>
      )}
    </div>
  );
}
