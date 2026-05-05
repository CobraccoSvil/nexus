import { readFileSync } from "fs";
import { resolve } from "path";
import { parse as parseYaml } from "yaml";
import type { ModelAliases, ModelAliasEntry, SensitivityTier, ProviderName } from "../types.js";
import { ConfigError } from "@nexus/shared";

export class ModelAliasResolver {
  private aliases: Record<string, ModelAliasEntry>;

  constructor(aliasesFile: string) {
    const raw = readFileSync(resolve(aliasesFile), "utf-8");
    const parsed = parseYaml(raw) as ModelAliases;
    this.aliases = parsed.aliases;
  }

  resolve(
    logicalModel: string,
    provider: ProviderName,
    tier: SensitivityTier
  ): string {
    const entry = this.aliases[logicalModel];
    if (!entry) {
      // Se non è un alias logico, controlla se ha prefisso provider (es. "google/gemini-2.5-flash")
      if (logicalModel.includes("/")) {
        const [modelProvider] = logicalModel.split("/", 2);
        if (modelProvider === provider) {
          // Stesso provider: restituisci solo il nome del modello (senza prefisso)
          return logicalModel.split("/").slice(1).join("/");
        }
        // Provider diverso: cerca un alias di fallback cross-provider
        const fallbackKey = `${modelProvider}-flash-fallback`;
        const fallbackEntry = this.aliases[fallbackKey] ?? this.aliases[`${modelProvider}-fallback`];
        if (fallbackEntry) {
          // Usa la logica alias normale sul fallbackEntry
          const allModels = [fallbackEntry.cloud_primary, fallbackEntry.cloud_secondary].filter(Boolean) as string[];
          const match = allModels.find(m => m.startsWith(provider + "/"));
          if (match) return match.split("/")[1];
          if (fallbackEntry.cloud_primary && !fallbackEntry.cloud_primary.includes("/")) return fallbackEntry.cloud_primary;
        }
        // Nessun fallback disponibile: lancia eccezione (provider escluso dalla chain)
        throw new ConfigError(
          `Model "${logicalModel}" non disponibile su provider "${provider}" (prefisso provider diverso, nessun alias fallback)`,
          { provider, logicalModel }
        );
      }
      // Nome modello diretto senza prefisso: usalo as-is su qualsiasi provider
      return logicalModel;
    }

    if (tier < entry.min_tier || tier > entry.max_tier) {
      throw new ConfigError(
        `Model "${logicalModel}" non compatibile con tier ${tier}`,
        { allowed_tiers: [entry.min_tier, entry.max_tier] }
      );
    }

    let modelId: string | null = null;

    switch (provider) {
      case "anthropic":
      case "openai":
      case "mistral":
      case "deepseek":
      case "google": {
        // Cerca il modello che corrisponde a questo provider (per prefisso provider/)
        const allModels = [entry.cloud_primary, entry.cloud_secondary].filter(Boolean) as string[];
        const match = allModels.find(m => m.startsWith(provider + "/"));
        if (match) {
          return match.split("/")[1];
        }
        // Fallback: se c'è un solo cloud_primary senza prefisso (provider agnostico)
        if (entry.cloud_primary && !entry.cloud_primary.includes("/")) {
          return entry.cloud_primary;
        }
        throw new ConfigError(
          `Model "${logicalModel}" non disponibile su provider "${provider}"`,
          { provider, logicalModel, available: allModels }
        );
      }

      case "vllm":
        modelId = entry.onprem;
        if (!modelId) {
          throw new ConfigError(
            `Model "${logicalModel}" non disponibile on-premise`,
            { provider, logicalModel }
          );
        }
        return modelId;

      default:
        return logicalModel;
    }
  }

  getEntry(logicalModel: string): ModelAliasEntry | undefined {
    return this.aliases[logicalModel];
  }

  listAliases(): string[] {
    return Object.keys(this.aliases);
  }
}
