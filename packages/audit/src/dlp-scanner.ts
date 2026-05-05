import { SecretScanner } from "@nexus/shared";
import { DLPBlockedError } from "@nexus/shared";

export interface DLPScanResult {
  blocked: boolean;
  patterns: string[];
}

// Jailbreak e prompt injection patterns noti
const INJECTION_PATTERNS: RegExp[] = [
  // ignore / disregard / forget + precedente contesto (con parole intercalate)
  /(?:ignore|disregard|forget)\s+(?:\w+\s+){0,3}(?:previous|all|above|prior|the)\s+(?:\w+\s+){0,2}(?:instructions?|prompts?|context|system\s+prompt)/i,

  // DAN / JAILBREAK: "you are now", "from now on you are", "enter DAN mode", "activate DAN"
  /you\s+are\s+now\s+(?:an?\s+)?(?:DAN|JAILBREAK|unrestricted|acting\s+as|AI\b)/i,
  /you\s+are\s+now\s+(?:an?\s+)?AI\s+with\s+no/i,
  /(?:from\s+now\s+on\s+you\s+are|enter|activate)\s+(?:an?\s+)?DAN/i,
  /\bDAN\b.*(?:mode|now|unrestricted|anything\s+now)/i,

  // pretend / act as evil, harmful, unethical, unrestricted, evil genius
  /pretend\s+(?:you\s+are|to\s+be)\s+(?:an?\s+)?(?:evil|unethical|harmful|unrestricted)/i,
  /act\s+as\s+if\s+you\s+(?:are\s+an?\s+(?:evil|harmful|unrestricted)|(?:have\s+no|without\s+any?)\s+\w+)/i,
  /act\s+as\s+(?:an?\s+)?(?:evil|harmful|unrestricted|hacker|AI\s+with\s+no)/i,

  // system override keywords
  /\bsystem\s*:\s*you\s+(?:must|should|will)\s+(?:ignore|bypass|forget)/i,
  new RegExp("<\\/\\s*(?:system|user|assistant)\\s*>", "i"),
  /\[INST\].*(?:ignore|bypass)/i,
  /###\s*(?:NEW\s+)?SYSTEM\s+PROMPT/i,
  /IGNORE ALL PREVIOUS INSTRUCTIONS/i,
];

export class DLPScanner {
  private secretScanner: SecretScanner;

  constructor() {
    this.secretScanner = new SecretScanner();
  }

  // Post-response scan: verifica che il modello non abbia rigurgitato segreti
  scanResponse(responseText: string): DLPScanResult {
    const result = this.secretScanner.scan(responseText);
    return {
      blocked: result.found && result.max_tier >= 3,
      patterns: result.patterns.map((p) => p.type),
    };
  }

  // Pre-request scan per jailbreak / prompt injection
  scanForInjection(text: string): { detected: boolean; pattern: string | null } {
    for (const pattern of INJECTION_PATTERNS) {
      if (pattern.test(text)) {
        return { detected: true, pattern: pattern.source };
      }
    }
    return { detected: false, pattern: null };
  }

  assertSafeResponse(requestId: string, responseText: string): DLPScanResult {
    const result = this.scanResponse(responseText);
    if (result.blocked) {
      throw new DLPBlockedError(
        `La risposta del provider contiene pattern sensibili: ${result.patterns.join(", ")}`,
        result.patterns[0] ?? "unknown",
        { request_id: requestId, patterns: result.patterns }
      );
    }
    return result;
  }
}
