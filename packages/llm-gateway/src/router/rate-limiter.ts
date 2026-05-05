import { RateLimitError } from "@nexus/shared";

interface RateLimitEntry {
  count: number;
  windowStart: number;
}

// Rate limiter in-memory con sliding window.
// In produzione sostituito da Redis (stesso contratto, implementazione diversa).
export class RateLimiter {
  private store = new Map<string, RateLimitEntry>();

  constructor(
    private limits: {
      perTenant: { requests: number; windowMs: number };
      perProvider: { requests: number; windowMs: number };
    }
  ) {}

  checkTenant(tenantId: string): void {
    this.check(
      `tenant:${tenantId}`,
      this.limits.perTenant.requests,
      this.limits.perTenant.windowMs,
      `Tenant "${tenantId}" ha superato il rate limit`
    );
  }

  checkProvider(providerName: string): void {
    this.check(
      `provider:${providerName}`,
      this.limits.perProvider.requests,
      this.limits.perProvider.windowMs,
      `Provider "${providerName}" ha raggiunto il rate limit`
    );
  }

  private check(key: string, maxReqs: number, windowMs: number, errorMsg: string): void {
    const now = Date.now();
    const entry = this.store.get(key);

    if (!entry || now - entry.windowStart >= windowMs) {
      this.store.set(key, { count: 1, windowStart: now });
      return;
    }

    if (entry.count >= maxReqs) {
      const retryAfterMs = windowMs - (now - entry.windowStart);
      throw new RateLimitError(errorMsg, retryAfterMs, {
        key,
        count: entry.count,
        limit: maxReqs,
        window_ms: windowMs,
      });
    }

    entry.count++;
  }

  // Cleanup periodico delle entry scadute
  cleanup(): void {
    const now = Date.now();
    for (const [key, entry] of this.store) {
      const limit =
        key.startsWith("tenant:")
          ? this.limits.perTenant.windowMs
          : this.limits.perProvider.windowMs;
      if (now - entry.windowStart >= limit) {
        this.store.delete(key);
      }
    }
  }
}
