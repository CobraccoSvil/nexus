export interface AnomalyEvent {
  type:
    | "token_spike"
    | "high_rate"
    | "injection_attempt"
    | "tier_escalation"
    | "unusual_finish_reason";
  severity: "low" | "medium" | "high";
  detail: string;
  tenant_id: string;
  request_id: string;
}

interface TenantStats {
  token_window: { count: number; start: number };
  request_window: { count: number; start: number };
  last_tiers: number[];
}

const WINDOW_MS = 60_000;           // finestra 1 min
const TOKEN_SPIKE_THRESHOLD = 50_000; // > 50k token/min = anomalia
const RATE_SPIKE_THRESHOLD = 200;     // > 200 req/min per tenant = anomalia
const TIER_HISTORY = 10;              // ultimi N tier per rilevare escalation

export class AnomalyDetector {
  private stats = new Map<string, TenantStats>();

  private getStats(tenantId: string): TenantStats {
    if (!this.stats.has(tenantId)) {
      this.stats.set(tenantId, {
        token_window: { count: 0, start: Date.now() },
        request_window: { count: 0, start: Date.now() },
        last_tiers: [],
      });
    }
    return this.stats.get(tenantId)!;
  }

  analyze(params: {
    tenant_id: string;
    request_id: string;
    input_tokens: number;
    output_tokens: number;
    sensitivity_tier: number;
    finish_reason: string;
    injection_detected: boolean;
  }): AnomalyEvent[] {
    const {
      tenant_id,
      request_id,
      input_tokens,
      output_tokens,
      sensitivity_tier,
      finish_reason,
      injection_detected,
    } = params;

    const events: AnomalyEvent[] = [];
    const now = Date.now();
    const stats = this.getStats(tenant_id);
    const totalTokens = input_tokens + output_tokens;

    // Reset finestre scadute
    if (now - stats.token_window.start >= WINDOW_MS) {
      stats.token_window = { count: 0, start: now };
    }
    if (now - stats.request_window.start >= WINDOW_MS) {
      stats.request_window = { count: 0, start: now };
    }

    stats.token_window.count += totalTokens;
    stats.request_window.count += 1;

    // 1. Token spike
    if (stats.token_window.count > TOKEN_SPIKE_THRESHOLD) {
      events.push({
        type: "token_spike",
        severity: "high",
        detail: `${stats.token_window.count} token nella finestra 1min (soglia: ${TOKEN_SPIKE_THRESHOLD})`,
        tenant_id,
        request_id,
      });
    }

    // 2. Rate spike
    if (stats.request_window.count > RATE_SPIKE_THRESHOLD) {
      events.push({
        type: "high_rate",
        severity: "medium",
        detail: `${stats.request_window.count} req/min (soglia: ${RATE_SPIKE_THRESHOLD})`,
        tenant_id,
        request_id,
      });
    }

    // 3. Injection attempt
    if (injection_detected) {
      events.push({
        type: "injection_attempt",
        severity: "high",
        detail: "Pattern di prompt injection rilevato",
        tenant_id,
        request_id,
      });
    }

    // 4. Tier escalation anomala (N richieste consecutive di tier crescente)
    stats.last_tiers.push(sensitivity_tier);
    if (stats.last_tiers.length > TIER_HISTORY) {
      stats.last_tiers.shift();
    }
    if (
      stats.last_tiers.length >= 5 &&
      stats.last_tiers.slice(-5).every((t, i, arr) => i === 0 || t >= arr[i - 1])
    ) {
      const max = Math.max(...stats.last_tiers.slice(-5));
      if (max >= 3) {
        events.push({
          type: "tier_escalation",
          severity: "medium",
          detail: `Ultimi ${TIER_HISTORY} tier in escalation progressiva fino a ${max}`,
          tenant_id,
          request_id,
        });
      }
    }

    // 5. Finish reason anomalo (content_filter = modello ha rifiutato)
    if (finish_reason === "content_filter") {
      events.push({
        type: "unusual_finish_reason",
        severity: "medium",
        detail: "Provider ha applicato content filter sulla risposta",
        tenant_id,
        request_id,
      });
    }

    return events;
  }

  resetTenant(tenantId: string): void {
    this.stats.delete(tenantId);
  }
}
