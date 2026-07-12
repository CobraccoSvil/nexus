// Logica PURA relativa all'icona provider (ADR 0037), senza React: testabile con
// `node --test`. Qui vive (regola L, punto unico) tutta la logica colore del
// provider: la NORMALIZZAZIONE del nome, i COLORI di brand, l'etichetta e la
// TINTA per-modello. provider-icon.tsx e provider-badge.tsx delegano a queste
// funzioni; nessun altro modulo re-implementa la stessa mappa.

/** Chiavi dei mark SVG disponibili. `generic` = fallback (iniziale in pallino). */
export type ProviderMarkKey =
  | "anthropic"
  | "openai"
  | "google"
  | "deepseek"
  | "mistral"
  | "generic";

/** Chiave di brand canonica del provider (include quelli senza logo ufficiale). */
export type ProviderBrandKey =
  | "anthropic"
  | "openai"
  | "google"
  | "deepseek"
  | "mistral"
  | "vllm"
  | "ollama"
  | "local"
  | "unknown";

/** Normalizzazione UNICA provider -> chiave di brand canonica (regola L). Alias
 *  comuni (claude, gpt, gemini, vertex, ...) mappati sul brand; provider ignoti
 *  -> "unknown". providerMarkKey / providerBaseColor / providerLabel e la tinta
 *  per-modello delegano tutti qui: un solo punto conosce gli alias. */
export function normalizeProviderKey(
  provider: string | null | undefined,
): ProviderBrandKey {
  const key = (provider ?? "").toLowerCase().trim();
  switch (key) {
    case "anthropic":
    case "claude":
      return "anthropic";
    case "openai":
    case "gpt":
    case "azure-openai":
      return "openai";
    case "google":
    case "gemini":
    case "vertex":
    case "vertex-ai":
      return "google";
    case "deepseek":
      return "deepseek";
    case "mistral":
      return "mistral";
    case "vllm":
      return "vllm";
    case "ollama":
      return "ollama";
    case "local":
      return "local";
    default:
      return "unknown";
  }
}

/** Normalizza il provider a una chiave di mark SVG. I provider senza logo
 *  ufficiale (vllm/ollama/local/ignoti) -> "generic" (iniziale in pallino).
 *  Nessun colore qui (quello e' in providerBaseColor / providerModelTint). */
export function providerMarkKey(
  provider: string | null | undefined,
): ProviderMarkKey {
  const norm = normalizeProviderKey(provider);
  switch (norm) {
    case "anthropic":
    case "openai":
    case "google":
    case "deepseek":
    case "mistral":
      return norm;
    default:
      return "generic";
  }
}

// ── Colori di brand per provider (punto unico, regola L) ────────────────────
// Coerenti con la barra status providers in alto a destra dell'IDE (OpenAI,
// Anthropic, Google, DeepSeek, Mistral). Hex base + etichetta leggibile.
// Riusati dal badge meta-step, dal nastro attivita' e dall'icona provider.
const PROVIDER_COLORS: Record<ProviderBrandKey, { base: string; label: string }> = {
  anthropic: { base: "#cc785c", label: "Anthropic" },
  openai: { base: "#10a37f", label: "OpenAI" },
  google: { base: "#4285f4", label: "Google" },
  deepseek: { base: "#7c3aed", label: "DeepSeek" },
  mistral: { base: "#ff7000", label: "Mistral" },
  vllm: { base: "#737373", label: "vLLM" },
  ollama: { base: "#737373", label: "Ollama" },
  local: { base: "#737373", label: "Local" },
  unknown: { base: "#94a3b8", label: "?" },
};

/** Colore brand base (#RRGGBB) per provider. Punto unico riusato dalle card
 *  meta-step, dal nastro attivita' e dall'icona provider per l'accento riga. */
export function providerBaseColor(provider: string | null | undefined): string {
  return PROVIDER_COLORS[normalizeProviderKey(provider)].base;
}

/** Etichetta leggibile del provider (brand). Per provider ignoti ritorna la
 *  sigla iniziale maiuscola (comportamento storico). */
export function providerLabel(provider: string | null | undefined): string {
  const norm = normalizeProviderKey(provider);
  if (norm !== "unknown") return PROVIDER_COLORS[norm].label;
  return provider ? provider.charAt(0).toUpperCase() + provider.slice(1) : "?";
}

// ── Helper colore puri (hex / HSL) ──────────────────────────────────────────

/** Converte hex (#RRGGBB) + alpha in `rgba(r,g,b,a)`. Punto unico riusato dal
 *  badge e dall'icona provider per tingere il mark. */
export function rgba(hex: string, alpha: number): string {
  const rgb = hexToRgb(hex);
  if (!rgb) return hex;
  return `rgba(${rgb.r},${rgb.g},${rgb.b},${alpha.toFixed(2)})`;
}

function hexToRgb(hex: string): { r: number; g: number; b: number } | null {
  const m = /^#?([0-9a-fA-F]{6})$/.exec(hex);
  if (!m) return null;
  const v = m[1];
  return {
    r: parseInt(v.slice(0, 2), 16),
    g: parseInt(v.slice(2, 4), 16),
    b: parseInt(v.slice(4, 6), 16),
  };
}

function rgbToHex(r: number, g: number, b: number): string {
  const to = (n: number): string =>
    Math.max(0, Math.min(255, Math.round(n))).toString(16).padStart(2, "0");
  return `#${to(r)}${to(g)}${to(b)}`;
}

/** RGB (0..255) -> HSL con h in [0,360), s/l in [0,1]. */
function rgbToHsl(
  r: number,
  g: number,
  b: number,
): { h: number; s: number; l: number } {
  const rn = r / 255;
  const gn = g / 255;
  const bn = b / 255;
  const max = Math.max(rn, gn, bn);
  const min = Math.min(rn, gn, bn);
  const l = (max + min) / 2;
  const d = max - min;
  if (d === 0) return { h: 0, s: 0, l };
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
  let h: number;
  if (max === rn) h = (gn - bn) / d + (gn < bn ? 6 : 0);
  else if (max === gn) h = (bn - rn) / d + 2;
  else h = (rn - gn) / d + 4;
  return { h: h * 60, s, l };
}

function hue2rgb(p: number, q: number, t: number): number {
  let tt = t;
  if (tt < 0) tt += 1;
  if (tt > 1) tt -= 1;
  if (tt < 1 / 6) return p + (q - p) * 6 * tt;
  if (tt < 1 / 2) return q;
  if (tt < 2 / 3) return p + (q - p) * (2 / 3 - tt) * 6;
  return p;
}

/** HSL (h in gradi, s/l in [0,1]) -> RGB (0..255). */
function hslToRgb(
  h: number,
  s: number,
  l: number,
): { r: number; g: number; b: number } {
  const hn = ((((h % 360) + 360) % 360) / 360);
  if (s === 0) {
    const v = l * 255;
    return { r: v, g: v, b: v };
  }
  const q = l < 0.5 ? l * (1 + s) : l + s - l * s;
  const p = 2 * l - q;
  return {
    r: hue2rgb(p, q, hn + 1 / 3) * 255,
    g: hue2rgb(p, q, hn) * 255,
    b: hue2rgb(p, q, hn - 1 / 3) * 255,
  };
}

/** Hash deterministico (FNV-1a 32 bit) di una stringa. Stabile cross-runtime:
 *  stesso input -> stesso valore ovunque. */
function hashString(s: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

function clamp01(v: number): number {
  return Math.max(0, Math.min(1, v));
}

function lerp(min: number, max: number, t: number): number {
  return min + (max - min) * t;
}

/**
 * TINTA per-modello (regola L: unico punto che compone brand + model).
 *
 * Parte dal colore di brand del provider e applica uno shift di HUE
 * DETERMINISTICO derivato da un hash del model id. Proprieta':
 *  - stesso (provider, model) -> stessa tinta (deterministica, no stato);
 *  - modelli diversi dello stesso provider -> tinte percettibilmente diverse;
 *  - lo shift e' CONTENUTO (default +-30 gradi) cosi' il brand resta
 *    riconoscibile (non si passa ad una famiglia di colore diversa);
 *  - piccole variazioni ortogonali di saturazione/luminosita' rendono distinti
 *    anche i grigi dei provider locali;
 *  - NON dipende dal costo: non collassa mai a un valore fisso a costo 0.
 *
 * La chiave della tinta e' provider (normalizzato) + model.
 *
 * @param options.maxHueShiftDeg ampiezza massima dello shift di hue (default 30).
 */
export function providerModelTint(
  provider: string | null | undefined,
  model?: string | null,
  options?: { maxHueShiftDeg?: number },
): string {
  const base = providerBaseColor(provider);
  const rgb = hexToRgb(base);
  if (!rgb || model == null || model === "") return base;
  const maxHue = options?.maxHueShiftDeg ?? 30;
  const h = hashString(`${normalizeProviderKey(provider)}:${model}`);
  // Tre canali indipendenti dallo stesso hash (bit disgiunti): hue dai 16 bit
  // bassi, saturazione/luminosita' da due byte alti (variazioni sottili).
  const hueShift = lerp(-maxHue, maxHue, (h & 0xffff) / 0xffff);
  const satShift = lerp(-0.08, 0.08, ((h >>> 16) & 0xff) / 0xff);
  const lightShift = lerp(-0.06, 0.06, ((h >>> 24) & 0xff) / 0xff);
  const hsl = rgbToHsl(rgb.r, rgb.g, rgb.b);
  const out = hslToRgb(
    hsl.h + hueShift,
    clamp01(hsl.s + satShift),
    clamp01(hsl.l + lightShift),
  );
  return rgbToHex(out.r, out.g, out.b);
}
