import type { SensitivityTier } from "../types.js";

export interface PresidioEntity {
  entity_type: string;
  start: number;
  end: number;
  score: number;
}

export interface PresidioResult {
  entities: PresidioEntity[];
  max_tier: SensitivityTier;
  has_pii: boolean;
}

// Mapping entity_type Presidio → tier di sensitivity
const ENTITY_TIER_MAP: Record<string, SensitivityTier> = {
  PERSON: 2,
  EMAIL_ADDRESS: 2,
  PHONE_NUMBER: 2,
  LOCATION: 1,
  DATE_TIME: 1,
  IT_FISCAL_CODE: 3,
  IT_DRIVER_LICENSE: 3,
  IT_VAT_CODE: 2,
  CREDIT_CARD: 3,
  IBAN_CODE: 3,
  MEDICAL_LICENSE: 3,
  NRP: 3,
  IP_ADDRESS: 2,
  URL: 1,
  CRYPTO: 3,
  US_SSN: 3,
  US_PASSPORT: 3,
  US_DRIVER_LICENSE: 3,
};

export class PresidioClient {
  private available = false;
  private lastCheck = 0;
  private checkIntervalMs = 30_000;

  constructor(private grpcUrl: string) {}

  private async checkAvailability(): Promise<boolean> {
    const now = Date.now();
    if (now - this.lastCheck < this.checkIntervalMs) {
      return this.available;
    }
    this.lastCheck = now;
    // In Phase 2 usiamo un health check HTTP semplice verso il REST wrapper di Presidio
    // Il microservizio Python espone anche /health su REST per questo
    try {
      const [host, port] = this.grpcUrl.split(":");
      const healthUrl = `http://${host}:${Number(port) + 1}/health`;
      const res = await fetch(healthUrl, { signal: AbortSignal.timeout(2000) });
      this.available = res.ok;
    } catch {
      this.available = false;
    }
    return this.available;
  }

  async analyze(text: string, language = "it"): Promise<PresidioResult> {
    const isUp = await this.checkAvailability();

    if (!isUp) {
      // Presidio non disponibile — ritorna risultato vuoto (il SecretScanner copre il gap)
      return { entities: [], max_tier: 0, has_pii: false };
    }

    try {
      const [host, port] = this.grpcUrl.split(":");
      const url = `http://${host}:${Number(port) + 1}/analyze`;

      const res = await fetch(url, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ text, language }),
        signal: AbortSignal.timeout(5000),
      });

      if (!res.ok) {
        return { entities: [], max_tier: 0, has_pii: false };
      }

      const entities: PresidioEntity[] = await res.json();
      let maxTier: SensitivityTier = 0;

      for (const e of entities) {
        const tier = ENTITY_TIER_MAP[e.entity_type] ?? 1;
        if (tier > maxTier) maxTier = tier as SensitivityTier;
      }

      return { entities, max_tier: maxTier, has_pii: entities.length > 0 };
    } catch {
      return { entities: [], max_tier: 0, has_pii: false };
    }
  }
}
