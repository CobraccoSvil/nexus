// Logica PURA del mapping provider -> chiave del mark SVG (ADR 0037), senza
// React: testabile con `node --test`. Il componente provider-icon.tsx la usa
// per scegliere quale mark disegnare.

/** Chiavi dei mark SVG disponibili. `generic` = fallback (iniziale in pallino). */
export type ProviderMarkKey =
  | "anthropic"
  | "openai"
  | "google"
  | "deepseek"
  | "mistral"
  | "generic";

/** Normalizza il provider a una chiave di mark. Alias comuni mappati sul brand
 *  noto; provider ignoti -> "generic". Nessun colore qui (quello e' in
 *  providerBaseColor, punto unico). */
export function providerMarkKey(provider: string | null | undefined): ProviderMarkKey {
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
    default:
      return "generic";
  }
}
