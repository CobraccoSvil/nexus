"use client";

// Icona compatta del PROVIDER per riga del nastro attivita' (ADR 0037): un mark
// SVG stilizzato riconoscibile, tinto col colore BRAND del provider e con una
// TONALITA' in base al costo del modello (cosi' modelli diversi dello stesso
// provider si distinguono). Il TOOLTIP mostra "Provider / model (· costo)".
//
// Regole L/G: colori/prezzi/alpha vengono dal punto unico provider-badge.tsx
// (providerBaseColor / providerLabel / usePricingCatalog / alphaFromCost / rgba);
// qui non c'e' alcun colore o prezzo hardcoded. I mark SVG sono semplici forme
// monocromatiche (currentColor) — non i loghi ufficiali, solo mark riconoscibili.
//
// Le bande "Cambio provider" NON usano questa icona (hanno i ProviderBadge).

import {
  providerBaseColor,
  providerLabel,
  usePricingCatalog,
  alphaFromCost,
  rgba,
} from "./provider-badge";
import { providerMarkKey, type ProviderMarkKey } from "./provider-icon-logic";

/** Mark SVG monocromatico (usa currentColor) per ogni provider noto. Forme
 *  semplici e riconoscibili, NON i loghi ufficiali. viewBox 0 0 24 24. */
function ProviderMark({ mark, initial }: { mark: ProviderMarkKey; initial: string }) {
  const common = { width: "100%", height: "100%", viewBox: "0 0 24 24", fill: "none" as const };
  switch (mark) {
    case "anthropic":
      // Mark stilizzato "A" a zampe divaricate (richiama il segno Anthropic).
      return (
        <svg {...common} aria-hidden>
          <path d="M8 19 12.2 5h2.1L18.5 19h-2.3l-1-3.1h-3.9L10.3 19H8Zm3.9-5h2.6l-1.3-4.2L11.9 14Z" fill="currentColor" />
          <path d="M5.5 19 9.7 5h1.2L6.7 19H5.5Z" fill="currentColor" opacity="0.55" />
        </svg>
      );
    case "openai":
      // Nodo esagonale/rosetta stilizzato.
      return (
        <svg {...common} aria-hidden>
          <path
            d="M12 3.5 19 7.75v8.5L12 20.5 5 16.25v-8.5L12 3.5Z"
            stroke="currentColor"
            strokeWidth="1.6"
            strokeLinejoin="round"
          />
          <circle cx="12" cy="12" r="3.1" stroke="currentColor" strokeWidth="1.6" />
        </svg>
      );
    case "google":
      // Arco a "G" aperto.
      return (
        <svg {...common} aria-hidden>
          <path
            d="M18 8.2A7 7 0 1 0 19 12h-7"
            stroke="currentColor"
            strokeWidth="1.9"
            strokeLinecap="round"
          />
        </svg>
      );
    case "deepseek":
      // Cerchio con punto (occhio/lente) stilizzato.
      return (
        <svg {...common} aria-hidden>
          <circle cx="12" cy="12" r="7.5" stroke="currentColor" strokeWidth="1.7" />
          <circle cx="12" cy="12" r="2.4" fill="currentColor" />
        </svg>
      );
    case "mistral":
      // Griglia a barre (richiama il mark a scacchiera Mistral).
      return (
        <svg {...common} aria-hidden>
          <rect x="4" y="5" width="4" height="4" fill="currentColor" />
          <rect x="10" y="5" width="4" height="4" fill="currentColor" opacity="0.75" />
          <rect x="16" y="5" width="4" height="4" fill="currentColor" opacity="0.5" />
          <rect x="4" y="15" width="4" height="4" fill="currentColor" opacity="0.5" />
          <rect x="10" y="15" width="4" height="4" fill="currentColor" opacity="0.75" />
          <rect x="16" y="15" width="4" height="4" fill="currentColor" />
        </svg>
      );
    default:
      // Fallback: iniziale del provider centrata.
      return (
        <svg {...common} aria-hidden>
          <text
            x="12"
            y="16.5"
            textAnchor="middle"
            fontSize="13"
            fontWeight="700"
            fill="currentColor"
            fontFamily="var(--font-mono)"
          >
            {initial}
          </text>
        </svg>
      );
  }
}

/**
 * Icona provider per una riga del nastro. Compatta (~15px), colore brand + tinta
 * costo, tooltip "Provider / model (· costo)".
 */
export function ProviderIcon({
  provider,
  model,
  size = 19,
}: {
  provider: string | null | undefined;
  model?: string | null;
  size?: number;
}) {
  const catalog = usePricingCatalog();
  if (!provider) return null;

  const mark = providerMarkKey(provider);
  const brand = providerBaseColor(provider);
  const label = providerLabel(provider);
  const entry =
    model != null
      ? catalog.find((e) => e.provider === provider && e.model === model) ?? null
      : null;
  // Riconoscibilita': il MARK e' sempre nel colore BRAND PIENO su un badge
  // circolare tinto del brand. Il costo del modello modula SOLO la densita' del
  // fondo (segnale secondario), cosi' modelli piu' costosi hanno il badge piu'
  // marcato ma l'icona resta sempre chiaramente leggibile.
  const alpha = alphaFromCost(entry);
  const markColor = brand;
  const bg = rgba(brand, 0.16 + Math.min(0.18, alpha * 0.18));
  const border = rgba(brand, 0.55);

  const tipParts: string[] = [`${label}${model ? ` / ${model}` : ""}`];
  if (entry?.input_cost_per_million_tokens != null) {
    tipParts.push(`in $${entry.input_cost_per_million_tokens.toFixed(2)}/M`);
  }
  if (entry?.output_cost_per_million_tokens != null) {
    tipParts.push(`out $${entry.output_cost_per_million_tokens.toFixed(2)}/M`);
  }
  const initial = (label || "?").charAt(0).toUpperCase();

  return (
    <span
      title={tipParts.join(" · ")}
      aria-label={`${label}${model ? ` ${model}` : ""}`}
      style={{
        display: "inline-flex",
        alignItems: "center",
        justifyContent: "center",
        width: size,
        height: size,
        boxSizing: "border-box",
        borderRadius: "50%",
        background: bg,
        border: `1px solid ${border}`,
        padding: Math.round(size * 0.16),
        color: markColor,
        flexShrink: 0,
        lineHeight: 0,
      }}
    >
      <ProviderMark mark={mark} initial={initial} />
    </span>
  );
}
