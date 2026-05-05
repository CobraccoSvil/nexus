import type { LLMMessage, SensitivityTier } from "../types.js";
import { SecretScanner } from "./secret-scanner.js";
import { PresidioClient } from "./presidio-client.js";

export interface ClassificationResult {
  tier: SensitivityTier;
  reasons: string[];
  secret_patterns: string[];
  presidio_entities: string[];
}

export class SensitivityClassifier {
  private scanner: SecretScanner;
  private presidio: PresidioClient;

  constructor(presidioGrpcUrl: string) {
    this.scanner = new SecretScanner();
    this.presidio = new PresidioClient(presidioGrpcUrl);
  }

  async classify(messages: LLMMessage[]): Promise<ClassificationResult> {
    const fullText = messages
      .map((m) => (typeof m.content === "string" ? m.content : JSON.stringify(m.content)))
      .join("\n");

    const reasons: string[] = [];
    const secretPatterns: string[] = [];
    const presidioEntities: string[] = [];
    let maxTier: SensitivityTier = 0;

    // 1. Secret scanner (sincrono, sempre disponibile)
    const scanResult = this.scanner.scan(fullText);
    if (scanResult.found) {
      for (const p of scanResult.patterns) {
        secretPatterns.push(p.type);
        if (p.tier > maxTier) maxTier = p.tier;
        reasons.push(`pattern ${p.type} (tier ${p.tier})`);
      }
    }

    // 2. Presidio PII detection (asincrono, fallback graceful se non disponibile)
    const presidioResult = await this.presidio.analyze(fullText);
    if (presidioResult.has_pii) {
      for (const e of presidioResult.entities) {
        presidioEntities.push(e.entity_type);
      }
      if (presidioResult.max_tier > maxTier) {
        maxTier = presidioResult.max_tier;
        reasons.push(`PII Presidio: ${presidioEntities.join(", ")} (tier ${presidioResult.max_tier})`);
      }
    }

    return {
      tier: maxTier,
      reasons,
      secret_patterns: secretPatterns,
      presidio_entities: presidioEntities,
    };
  }

  // Versione sincrona senza Presidio — per path veloci dove la latency è critica
  classifySync(messages: LLMMessage[]): ClassificationResult {
    const fullText = messages
      .map((m) => (typeof m.content === "string" ? m.content : JSON.stringify(m.content)))
      .join("\n");

    const reasons: string[] = [];
    const secretPatterns: string[] = [];
    let maxTier: SensitivityTier = 0;

    const scanResult = this.scanner.scan(fullText);
    if (scanResult.found) {
      for (const p of scanResult.patterns) {
        secretPatterns.push(p.type);
        if (p.tier > maxTier) maxTier = p.tier;
        reasons.push(`pattern ${p.type} (tier ${p.tier})`);
      }
    }

    return { tier: maxTier, reasons, secret_patterns: secretPatterns, presidio_entities: [] };
  }
}
