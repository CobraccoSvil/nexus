import { readFileSync } from "fs";
import { resolve } from "path";
import { parse as parseYaml } from "yaml";
import type { SensitivityTier, ProviderName } from "../types.js";
import { ProviderError } from "@nexus/shared";

interface TierPolicy {
  primary?: ProviderName;
  secondary?: ProviderName;
  tertiary?: ProviderName;
  fallback?: ProviderName;
  blocked?: boolean;
}

interface PolicyFile {
  profile: string;
  routing: {
    tier_0: TierPolicy;
    tier_1: TierPolicy;
    tier_2: TierPolicy;
    tier_3: TierPolicy;
  };
  features?: {
    allow_cloud_tier2?: boolean;
    allow_cloud_tier3?: boolean;
  };
}

export interface RoutingDecision {
  providers: ProviderName[];
  blocked: boolean;
  reason?: string;
}

export class PolicyEngine {
  private policy: PolicyFile;

  constructor(policyFile: string) {
    const raw = readFileSync(resolve(policyFile), "utf-8");
    this.policy = parseYaml(raw) as PolicyFile;
  }

  decide(
    tier: SensitivityTier,
    feature: string,
    tenantFlags: Record<string, boolean> = {}
  ): RoutingDecision {
    const tierKey = `tier_${tier}` as keyof PolicyFile["routing"];
    const tierPolicy = this.policy.routing?.[tierKey];

    if (!tierPolicy) {
      return {
        providers: [],
        blocked: true,
        reason: `Nessuna policy per tier ${tier}`,
      };
    }

    if (tierPolicy.blocked) {
      return {
        providers: [],
        blocked: true,
        reason: `Tier ${tier} bloccato dalla policy (profilo: ${this.policy.profile})`,
      };
    }

    // Tenant override: se il tenant ha allow_cloud disabilitato, esclude provider cloud
    const tenantBlocksCloud = tenantFlags["block_cloud"] === true;
    const cloudProviders: ProviderName[] = ["anthropic", "openai", "mistral", "deepseek", "google"];

    const ordered: ProviderName[] = [
      tierPolicy.primary,
      tierPolicy.secondary,
      tierPolicy.tertiary,
      tierPolicy.fallback,
    ].filter((n): n is ProviderName => !!n);

    const filtered = tenantBlocksCloud
      ? ordered.filter((p) => !cloudProviders.includes(p))
      : ordered;

    if (filtered.length === 0) {
      return {
        providers: [],
        blocked: true,
        reason: `Nessun provider disponibile per tier ${tier} con i flag tenant correnti`,
      };
    }

    return { providers: filtered, blocked: false };
  }

  getProfile(): string {
    return this.policy.profile;
  }

  // Valida che il tier assegnato dal classifier sia <= tier dichiarato nella request metadata.
  // La request deve dichiarare un tier almeno pari a quello rilevato — non può sottodichiarare.
  validateTierClaim(claimed: SensitivityTier, detected: SensitivityTier): void {
    if (detected > claimed) {
      throw new ProviderError(
        `Tier dichiarato (${claimed}) inferiore al tier rilevato (${detected}). ` +
          `Il prompt contiene dati di sensibilità ${detected} e deve essere inviato con tier >= ${detected}.`,
        "policy-engine",
        400,
        { claimed_tier: claimed, detected_tier: detected }
      );
    }
  }
}
