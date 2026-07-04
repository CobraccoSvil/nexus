"use client";

// Icona del PROVIDER per riga del nastro attivita' (ADR 0037): il LOGO UFFICIALE
// del provider come SVG inline, dentro un badge circolare tinto del brand.
//  - Loghi MONOCROMATICI (OpenAI, Anthropic, DeepSeek, Mistral): resi nel colore
//    BRAND del provider via currentColor (cosi' sono visibili su tema chiaro/scuro).
//  - Logo MULTICOLORE (Google Gemini "sparkle"): reso coi suoi colori ufficiali.
// La TONALITA' del badge dipende dal costo del modello. Il TOOLTIP mostra
// "Provider / model (· costo)".
//
// Regole L/G: colori/prezzi/alpha vengono dal punto unico provider-badge.tsx
// (providerBaseColor / providerLabel / usePricingCatalog / alphaFromCost / rgba);
// qui nessun colore/prezzo hardcoded oltre ai colori PROPRI dei loghi ufficiali.
// I loghi sono marchi dei rispettivi provider, usati come indicatori funzionali.
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

// Loghi ufficiali (SVG inline, markup interno). `colorful`=true: il markup porta
// i propri colori (nessuna tinta brand applicata). Altrimenti fill="currentColor"
// -> il colore brand del provider e' applicato dal contenitore.
const LOGOS: Record<
  Exclude<ProviderMarkKey, "generic">,
  { viewBox: string; inner: string; colorful: boolean }
> = {
  openai: {
    viewBox: "0 0 24 24",
    colorful: false,
    inner: `<path fill="currentColor" d="M22.2819 9.8211a5.9847 5.9847 0 0 0-.5157-4.9108 6.0462 6.0462 0 0 0-6.5098-2.9A6.0651 6.0651 0 0 0 4.9807 4.1818a5.9847 5.9847 0 0 0-3.9977 2.9 6.0462 6.0462 0 0 0 .7427 7.0966 5.98 5.98 0 0 0 .511 4.9107 6.051 6.051 0 0 0 6.5146 2.9001A5.9847 5.9847 0 0 0 13.2599 24a6.0557 6.0557 0 0 0 5.7718-4.2058 5.9894 5.9894 0 0 0 3.9977-2.9001 6.0557 6.0557 0 0 0-.7475-7.0729zm-9.022 12.6081a4.4755 4.4755 0 0 1-2.8764-1.0408l.1419-.0804 4.7783-2.7582a.7948.7948 0 0 0 .3927-.6813v-6.7369l2.02 1.1686a.071.071 0 0 1 .038.052v5.5826a4.504 4.504 0 0 1-4.4945 4.4944zm-9.6607-4.1254a4.4708 4.4708 0 0 1-.5346-3.0137l.142.0852 4.783 2.7582a.7712.7712 0 0 0 .7806 0l5.8428-3.3685v2.3324a.0804.0804 0 0 1-.0332.0615L9.74 19.9502a4.4992 4.4992 0 0 1-6.1408-1.6464zM2.3408 7.8956a4.485 4.485 0 0 1 2.3655-1.9728V11.6a.7664.7664 0 0 0 .3879.6765l5.8144 3.3543-2.0201 1.1685a.0757.0757 0 0 1-.071 0l-4.8303-2.7865A4.504 4.504 0 0 1 2.3408 7.872zm16.5963 3.8558L13.1038 8.364 15.1192 7.2a.0757.0757 0 0 1 .071 0l4.8303 2.7913a4.4944 4.4944 0 0 1-.6765 8.1042v-5.6772a.79.79 0 0 0-.407-.667zm2.0107-3.0231l-.142-.0852-4.7735-2.7818a.7759.7759 0 0 0-.7854 0L9.409 9.2297V6.8974a.0662.0662 0 0 1 .0284-.0615l4.8303-2.7866a4.4992 4.4992 0 0 1 6.6802 4.66zM8.3065 12.863l-2.02-1.1638a.0804.0804 0 0 1-.038-.0567V6.0742a4.4992 4.4992 0 0 1 7.3757-3.4537l-.142.0805L8.704 5.459a.7948.7948 0 0 0-.3927.6813zm1.0976-2.3654l2.602-1.4998 2.6069 1.4998v2.9994l-2.5974 1.4997-2.6067-1.4997Z"/>`,
  },
  anthropic: {
    viewBox: "0 0 24 24",
    colorful: false,
    inner: `<path fill="currentColor" d="M17.3041 3.541h-3.6718l6.696 16.918H24Zm-10.6082 0L0 20.459h3.7442l1.3693-3.5527h7.0052l1.3693 3.5528h3.7442L10.5363 3.5409Zm-.3712 10.2232 2.2914-5.9456 2.2914 5.9456Z"/>`,
  },
  google: {
    viewBox: "0 0 28 28",
    colorful: true,
    inner: `<path d="M14 28C14 26.0633 13.6267 24.2433 12.88 22.54C12.1567 20.8367 11.165 19.355 9.905 18.095C8.645 16.835 7.16333 15.8433 5.46 15.12C3.75667 14.3733 1.93667 14 0 14C1.93667 14 3.75667 13.6383 5.46 12.915C7.16333 12.1683 8.645 11.165 9.905 9.905C11.165 8.645 12.1567 7.16333 12.88 5.46C13.6267 3.75667 14 1.93667 14 0C14 1.93667 14.3617 3.75667 15.085 5.46C15.8317 7.16333 16.835 8.645 18.095 9.905C19.355 11.165 20.8367 12.1683 22.54 12.915C24.2433 13.6383 26.0633 14 28 14C26.0633 14 24.2433 14.3733 22.54 15.12C20.8367 15.8433 19.355 16.835 18.095 18.095C16.835 19.355 15.8317 20.8367 15.085 22.54C14.3617 24.2433 14 26.0633 14 28Z" fill="url(#google-gemini-sparkle)"/><defs><radialGradient id="google-gemini-sparkle" cx="0" cy="0" r="1" gradientUnits="userSpaceOnUse" gradientTransform="translate(2.77876 11.3795) rotate(18.6832) scale(29.8025 238.737)"><stop offset="0.0671246" stop-color="#9168C0"/><stop offset="0.342551" stop-color="#5684D1"/><stop offset="0.672076" stop-color="#1BA1E3"/></radialGradient></defs>`,
  },
  deepseek: {
    viewBox: "0 0 24 24",
    colorful: false,
    inner: `<path fill="currentColor" d="M23.748 4.651c-.254-.124-.364.113-.512.233-.051.04-.094.09-.137.137-.372.397-.806.657-1.373.626-.829-.046-1.537.214-2.163.848-.133-.782-.575-1.248-1.247-1.548-.352-.155-.708-.311-.955-.65-.172-.24-.219-.509-.305-.774-.055-.16-.11-.323-.293-.35-.2-.031-.278.136-.356.276-.313.572-.434 1.202-.422 1.84.027 1.436.633 2.58 1.838 3.393.137.094.172.187.129.323-.082.28-.18.553-.266.833-.055.179-.137.218-.328.14a5.5 5.5 0 0 1-1.737-1.179c-.857-.828-1.631-1.743-2.597-2.46a12 12 0 0 0-.689-.47c-.985-.957.13-1.743.387-1.836.27-.098.094-.433-.778-.428-.872.003-1.67.295-2.687.685a3 3 0 0 1-.465.136 9.6 9.6 0 0 0-2.883-.101c-1.885.21-3.39 1.1-4.497 2.622C.082 8.776-.231 10.854.152 13.02c.403 2.284 1.568 4.175 3.36 5.653 1.857 1.533 3.997 2.284 6.438 2.14 1.482-.085 3.132-.284 4.994-1.86.47.234.962.328 1.78.398.629.058 1.235-.031 1.705-.129.735-.155.684-.836.418-.961-2.155-1.004-1.682-.595-2.112-.926 1.095-1.295 2.768-3.598 3.284-6.733.05-.346.115-.834.108-1.114-.004-.171.035-.238.23-.257a4.2 4.2 0 0 0 1.545-.475c1.397-.763 1.96-2.016 2.093-3.517.02-.23-.004-.467-.247-.588M11.58 18.168c-2.088-1.642-3.101-2.183-3.52-2.16-.39.024-.32.472-.234.763.09.288.207.487.371.74.114.167.192.416-.113.603-.673.416-1.842-.14-1.897-.168-1.361-.801-2.5-1.86-3.301-3.306-.775-1.393-1.225-2.888-1.299-4.482-.02-.385.094-.522.477-.592a4.7 4.7 0 0 1 1.53-.038c2.131.311 3.946 1.264 5.467 2.774.868.86 1.525 1.887 2.202 2.89.72 1.066 1.494 2.082 2.48 2.915.348.291.626.513.892.677-.802.09-2.14.109-3.055-.615zm1.001-6.44a.306.306 0 0 1 .415-.287.3.3 0 0 1 .113.074.3.3 0 0 1 .086.214c0 .17-.136.307-.308.307a.303.303 0 0 1-.306-.307m3.11 1.596c-.2.081-.4.151-.591.16a1.25 1.25 0 0 1-.798-.254c-.274-.23-.47-.358-.551-.758a1.7 1.7 0 0 1 .015-.588c.07-.327-.007-.537-.238-.727-.188-.156-.426-.199-.689-.199a.6.6 0 0 1-.254-.078.253.253 0 0 1-.114-.358 1 1 0 0 1 .192-.21c.356-.202.767-.136 1.146.016.352.144.618.408 1.001.782.392.451.462.576.685.915.176.264.336.536.446.848.066.194-.02.353-.25.45"/>`,
  },
  mistral: {
    viewBox: "0 0 24 24",
    colorful: false,
    inner: `<path fill="currentColor" d="M17.143 3.429v3.428h-3.429v3.429h-3.428V6.857H6.857V3.43H3.43v13.714H0v3.428h10.286v-3.428H6.857v-3.429h3.429v3.429h3.429v-3.429h3.428v3.429h-3.428v3.428H24v-3.428h-3.43V3.429z"/>`,
  },
};

/** Logo ufficiale del provider (SVG inline). Fallback: iniziale del provider. */
function ProviderMark({ mark, initial }: { mark: ProviderMarkKey; initial: string }) {
  const logo = mark !== "generic" ? LOGOS[mark] : undefined;
  if (!logo) {
    return (
      <svg width="100%" height="100%" viewBox="0 0 24 24" fill="none" aria-hidden>
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
  return (
    <svg
      width="100%"
      height="100%"
      viewBox={logo.viewBox}
      aria-hidden
      // Markup SVG STATICO e fidato (loghi hardcoded, nessun input utente).
      dangerouslySetInnerHTML={{ __html: logo.inner }}
    />
  );
}

/**
 * Icona provider per una riga del nastro: logo ufficiale (~20px) in un badge
 * circolare tinto del brand, tooltip "Provider / model (· costo)".
 */
export function ProviderIcon({
  provider,
  model,
  size = 20,
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
  // Tonalita' del badge in base al costo del modello (segnale secondario). Il
  // LOGO monocromatico e' sempre nel colore brand pieno (leggibile ovunque);
  // il logo multicolore (Gemini) mantiene i suoi colori.
  const alpha = alphaFromCost(entry);
  const bg = rgba(brand, 0.12 + Math.min(0.16, alpha * 0.16));
  const border = rgba(brand, 0.5);

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
        padding: Math.round(size * 0.15),
        color: brand,
        flexShrink: 0,
        lineHeight: 0,
      }}
    >
      <ProviderMark mark={mark} initial={initial} />
    </span>
  );
}
