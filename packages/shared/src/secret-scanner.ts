import type { SensitivityTier } from "./llm-types.js";

export interface ScanResult {
  found: boolean;
  patterns: FoundPattern[];
  max_tier: SensitivityTier;
}

export interface FoundPattern {
  type: PatternType;
  tier: SensitivityTier;
  offset: number;
  length: number;
}

export type PatternType =
  | "aws_key"
  | "aws_secret"
  | "gcp_service_account"
  | "azure_sas"
  | "github_pat"
  | "gitlab_token"
  | "jwt"
  | "pem_private_key"
  | "db_connection_string"
  | "generic_api_key"
  | "italian_cf"
  | "italian_iban"
  | "credit_card"
  | "email_pii";

interface PatternDef {
  type: PatternType;
  pattern: RegExp;
  tier: SensitivityTier;
}

const PATTERNS: PatternDef[] = [
  {
    type: "aws_key",
    pattern: /(?<![A-Z0-9])(AKIA|ABIA|ACCA|ASIA)[A-Z0-9]{16}(?![A-Z0-9])/,
    tier: 3,
  },
  {
    // Una AWS secret key e' una stringa di 40 char base64. Matchare QUALSIASI
    // sequenza di 40 char produce falsi positivi su hash SHA, UUID, blob id,
    // contenuti base64 (es. payload Figma ai_chat.json) -> tier 3 spurio ->
    // blocco DLP del prompt. Richiediamo quindi il CONTESTO di un nome campo
    // AWS (aws_secret_access_key / secret_access_key / aws_access...) prima
    // del valore: i veri secret appaiono sempre in quel contesto, gli hash
    // generici no. Fix classifier "meno aggressivo".
    type: "aws_secret",
    pattern: /(?:aws_?secret_?access_?key|secret_?access_?key|aws_?secret)["'\s:=]{0,12}[A-Za-z0-9/+=]{40}(?![A-Za-z0-9/+=])/i,
    tier: 3,
  },
  {
    type: "gcp_service_account",
    pattern: /"type"\s*:\s*"service_account"/,
    tier: 3,
  },
  {
    type: "azure_sas",
    pattern: /SharedAccessSignature\s+sig=[A-Za-z0-9%+/=]+/,
    tier: 3,
  },
  {
    type: "github_pat",
    pattern: /gh[pousr]_[A-Za-z0-9_]{20,255}/,
    tier: 3,
  },
  {
    type: "gitlab_token",
    pattern: /glpat-[A-Za-z0-9\-_]{20,}/,
    tier: 3,
  },
  {
    type: "pem_private_key",
    pattern: /-----BEGIN\s+(RSA|EC|DSA|OPENSSH)?\s*PRIVATE KEY-----/,
    tier: 3,
  },
  {
    type: "db_connection_string",
    pattern: /(?:postgres(?:ql)?|mysql|mongodb|redis|mssql):\/\/[^@\s]+:[^@\s]+@[^\s]+/i,
    tier: 3,
  },
  {
    type: "jwt",
    pattern: /eyJ[A-Za-z0-9_-]+\.eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+/,
    tier: 2,
  },
  {
    type: "generic_api_key",
    pattern: /(?:api[_-]?key|api[_-]?secret|access[_-]?token|bearer)\s*[=:]\s*["']?[A-Za-z0-9_\-.]{20,}["']?/i,
    tier: 2,
  },
  {
    type: "italian_cf",
    pattern: /\b[A-Z]{6}\d{2}[A-Z]\d{2}[A-Z]\d{3}[A-Z]\b/i,
    tier: 3,
  },
  {
    type: "italian_iban",
    pattern: /\bIT\d{2}\s?[A-Z]\d{3}\s?\d{4}\s?\d{4}\s?\d{4}\s?\d{4}\s?\d{3}\b/i,
    tier: 3,
  },
  {
    type: "credit_card",
    pattern: /\b(?:4[0-9]{12}(?:[0-9]{3})?|5[1-5][0-9]{14}|3[47][0-9]{13}|3(?:0[0-5]|[68][0-9])[0-9]{11})\b/,
    tier: 3,
  },
  {
    type: "email_pii",
    pattern: /\b[a-zA-Z0-9._%+\-]+@[a-zA-Z0-9.\-]+\.[a-zA-Z]{2,}\b/,
    tier: 2,
  },
];

export class SecretScanner {
  scan(text: string): ScanResult {
    const found: FoundPattern[] = [];
    let maxTier: SensitivityTier = 0;

    for (const def of PATTERNS) {
      const match = def.pattern.exec(text);
      if (match) {
        found.push({
          type: def.type,
          tier: def.tier,
          offset: match.index,
          length: match[0].length,
        });
        if (def.tier > maxTier) maxTier = def.tier;
      }
    }

    return {
      found: found.length > 0,
      patterns: found,
      max_tier: maxTier,
    };
  }

  redact(text: string): { redacted: string; count: number } {
    let redacted = text;
    let count = 0;

    for (const def of PATTERNS) {
      const before = redacted;
      redacted = redacted.replace(
        new RegExp(def.pattern.source, def.pattern.flags + "g"),
        `[REDACTED:${def.type}]`
      );
      if (redacted !== before) count++;
    }

    return { redacted, count };
  }
}
