import type { LLMProvider, LLMRequest, LLMResponse, ProviderStatus } from "../types.js";
import { ProviderError } from "@nexus/shared";

export class FallbackChain {
  public modelPerProvider: Map<string, string> | null = null;

  constructor(
    private providers: LLMProvider[],
    private statuses: Map<string, ProviderStatus>
  ) {}

  /** Rimuove provider incompatibili per questa richiesta (es. model alias non risolvibile). */
  filterInPlace(predicate: (p: LLMProvider) => boolean) {
    this.providers = this.providers.filter(predicate);
  }

  async complete(req: LLMRequest): Promise<LLMResponse & { attempts: number }> {
    let lastError: Error | null = null;
    let attempts = 0;

    for (const provider of this.providers) {
      const status = this.statuses.get(provider.name);
      if (status && !status.healthy) {
        continue;
      }

      try {
        attempts++;
        const providerReq = this.modelPerProvider
          ? { ...req, model: this.modelPerProvider.get(provider.name) ?? req.model }
          : req;
        const result = await provider.complete(providerReq);
        return { ...result, attempts };
      } catch (err) {
        lastError = err instanceof Error ? err : new Error(String(err));
        // Marca il provider come unhealthy temporaneamente
        const current = this.statuses.get(provider.name);
        if (current) {
          this.statuses.set(provider.name, {
            ...current,
            healthy: false,
            last_error: lastError.message,
          });
          // Ripristina dopo 60 secondi
          setTimeout(() => {
            const s = this.statuses.get(provider.name);
            if (s) this.statuses.set(provider.name, { ...s, healthy: true });
          }, 30_000);
        }
      }
    }

    throw new ProviderError(
      `Tutti i provider hanno fallito. Ultimo errore: ${lastError?.message}`,
      "fallback-chain",
      503,
      { attempts }
    );
  }
}
