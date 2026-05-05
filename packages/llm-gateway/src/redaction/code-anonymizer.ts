import type { RedactionMap } from "./redaction-map.js";

// Anonimizza codice sorgente nei prompt.
// Fase 3: regex-based. Fase 4+: tree-sitter AST per parsing strutturato.
//
// Rimuove:
// 1. Identificatori annotati con @confidential (variabili, funzioni, classi)
// 2. String literals che sembrano segreti (> 20 char, entropia alta)
// 3. Commenti inline con dati sensibili (TODO/FIXME con valori)

const CONFIDENTIAL_ANNOTATION = /@confidential\s*\n\s*(?:const|let|var|function|class|def|private|public|protected)\s+(\w+)/g;

// Pattern per string literals con potenziale token/segreto (alta entropia, >20 char)
// Usa new RegExp per evitare ambiguità col backreference dentro literal
const SECRET_STRING_LITERAL = new RegExp(
  `(["'\`])[A-Za-z0-9+/=_\\-]{20,}\\1`,
  "g"
);

// Pattern per valori inline dopo = che sembrano token (tre varianti per ogni tipo di virgoletta)
const INLINE_SECRET_DOUBLE = /(?:password|secret|token|key|api_?key|access_?key)\s*[:=]\s*"([^"]{8,})"/gi;
const INLINE_SECRET_SINGLE = /(?:password|secret|token|key|api_?key|access_?key)\s*[:=]\s*'([^']{8,})'/gi;
const INLINE_SECRET_BACKTICK = /(?:password|secret|token|key|api_?key|access_?key)\s*[:=]\s*`([^`]{8,})`/gi;

export interface AnonymizationResult {
  text: string;
  count: number;
  types: string[];
}

export class CodeAnonymizer {
  anonymize(text: string, map: RedactionMap): AnonymizationResult {
    let result = text;
    let count = 0;
    const types: string[] = [];

    // 1. Identificatori @confidential
    const confMatches: { full: string; name: string }[] = [];
    let m: RegExpExecArray | null;
    const confRegex = new RegExp(CONFIDENTIAL_ANNOTATION.source, "g");
    while ((m = confRegex.exec(text)) !== null) {
      confMatches.push({ full: m[0], name: m[1] });
    }
    for (const { name } of confMatches) {
      const placeholder = map.store(name, "identifier");
      // Sostituisce tutti gli usi dell'identificatore nel testo
      result = result.replaceAll(
        new RegExp(`\\b${escapeRegex(name)}\\b`, "g"),
        placeholder
      );
      count++;
      if (!types.includes("identifier")) types.push("identifier");
    }

    // 2. Assignment inline segreti (doppi apici, singoli, backtick)
    const replaceSecret = (match: string, inner: string, quote: string): string => {
      const placeholder = map.store(inner, "secret_value");
      count++;
      if (!types.includes("secret_value")) types.push("secret_value");
      return match.replace(`${quote}${inner}${quote}`, `${quote}${placeholder}${quote}`);
    };

    result = result.replace(INLINE_SECRET_DOUBLE, (m, v) => replaceSecret(m, v, '"'));
    result = result.replace(INLINE_SECRET_SINGLE, (m, v) => replaceSecret(m, v, "'"));
    result = result.replace(INLINE_SECRET_BACKTICK, (m, v) => replaceSecret(m, v, "`"));

    // 3. String literals ad alta entropia (potenziali token)
    result = result.replace(SECRET_STRING_LITERAL, (match, quote) => {
      const inner = match.slice(1, -1);
      if (looksLikeSecret(inner)) {
        const placeholder = map.store(inner, "high_entropy_string");
        count++;
        if (!types.includes("high_entropy_string")) types.push("high_entropy_string");
        return `${quote}${placeholder}${quote}`;
      }
      return match;
    });

    return { text: result, count, types };
  }
}

function escapeRegex(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function looksLikeSecret(s: string): boolean {
  if (s.length < 20) return false;
  // Calcola entropia di Shannon approssimata
  const freq = new Map<string, number>();
  for (const c of s) freq.set(c, (freq.get(c) ?? 0) + 1);
  let entropy = 0;
  for (const count of freq.values()) {
    const p = count / s.length;
    entropy -= p * Math.log2(p);
  }
  // Entropia > 4.0 su stringa > 20 char = probabile token/hash
  return entropy > 4.0;
}
