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
    dlp_enabled?: boolean;
  };
}

export interface RoutingDecision {
  providers: ProviderName[];
  blocked: boolean;
  reason?: string;
}

/**
 * Interfaccia minimale per il client DB (lib `postgres`). Evita una dipendenza
 * diretta dal package gateway sul driver: il chiamante (nexus-gateway) inietta
 * il proprio `sql` gia' configurato. La firma e' volutamente lasca (return
 * `any`) per restare strutturalmente compatibile con i numerosi overload del
 * tipo `Sql` di `postgres` (tagged template + helper); il consumo concreto e'
 * tipizzato a destinazione in `refreshDbOverrides`.
 */
// Return volutamente lasco (`any`): il client `postgres` espone numerosi
// overload (tagged template + helper `Sql`) che non si lasciano vincolare a un
// `Promise<T[]>` esatto. Il tipo concreto delle righe e' applicato in
// `refreshDbOverrides` tramite cast. Il package non ha lint eslint attivo
// (nessuno script `lint` in package.json), quindi `any` qui e' accettabile e
// confinato a questo confine di iniezione DB.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type SettingsDb = (strings: TemplateStringsArray, ...values: unknown[]) => any;

const PARSE_BOOL = (v: string | undefined): boolean | undefined => {
  if (v === undefined || v === null) return undefined;
  const t = String(v).trim().toLowerCase();
  if (t === "true" || t === "1") return true;
  if (t === "false" || t === "0") return false;
  return undefined;
};

export class PolicyEngine {
  private policy: PolicyFile;

  // Provider considerati cloud (inviano dati a servizi esterni). Fonte unica
  // usata sia per il filtro tenant sia per l'enforcement dei flag DLP per-tier.
  private static readonly CLOUD_PROVIDERS: ProviderName[] = [
    "anthropic",
    "openai",
    "mistral",
    "deepseek",
    "google",
  ];

  // Override letti dai settings DB (regola G: configurazione dal DB, non dal
  // file YAML). `undefined` = nessun valore DB letto -> usa il default YAML.
  private dbOverrides: {
    allow_cloud_tier2?: boolean;
    allow_cloud_tier3?: boolean;
    dlp_enabled?: boolean;
  } = {};
  private dbOverridesFetchedAt = 0;
  private static readonly DB_TTL_MS = 60_000;

  constructor(policyFile: string) {
    const raw = readFileSync(resolve(policyFile), "utf-8");
    this.policy = parseYaml(raw) as PolicyFile;
  }

  /**
   * Valore effettivo di un flag: priorita' al DB (settings), fallback YAML.
   */
  private flag(key: "allow_cloud_tier2" | "allow_cloud_tier3" | "dlp_enabled"): boolean | undefined {
    const fromDb = this.dbOverrides[key];
    if (fromDb !== undefined) return fromDb;
    return this.policy.features?.[key];
  }

  /**
   * Indica se il tier richiede l'enforcement del flag cloud DLP e qual e' il
   * flag corrispondente. Tier 0/1 non hanno gate cloud (sempre permessi).
   */
  private cloudGateForTier(tier: SensitivityTier): boolean | undefined {
    if (tier >= 3) return this.flag("allow_cloud_tier3");
    if (tier === 2) return this.flag("allow_cloud_tier2");
    return undefined; // tier 0/1: nessun gate cloud
  }

  /**
   * Ricarica i flag DLP dai settings DB con cache TTL 60s. Se il DB e'
   * irraggiungibile mantiene i valori correnti (fallback graceful sullo YAML).
   * `force` ignora la cache (usato all'avvio).
   */
  async refreshDbOverrides(db: SettingsDb | null | undefined, force = false): Promise<void> {
    if (!db) return;
    const now = Date.now();
    if (!force && now - this.dbOverridesFetchedAt < PolicyEngine.DB_TTL_MS) return;

    try {
      const rows = (await db`
        SELECT key, value FROM settings
        WHERE key IN ('dlp_allow_cloud_tier2', 'dlp_allow_cloud_tier3', 'dlp_enabled')
      `) as Array<{ key: string; value: string }>;
      const map = new Map(rows.map((r) => [r.key, r.value]));
      const next: typeof this.dbOverrides = {};
      next.allow_cloud_tier2 = PARSE_BOOL(map.get("dlp_allow_cloud_tier2"));
      next.allow_cloud_tier3 = PARSE_BOOL(map.get("dlp_allow_cloud_tier3"));
      next.dlp_enabled = PARSE_BOOL(map.get("dlp_enabled"));
      this.dbOverrides = next;
      this.dbOverridesFetchedAt = now;
    } catch (err) {
      // DB down: non azzerare gli override gia' noti; il routing prosegue sui
      // valori YAML / ultimi noti. Timestamp non aggiornato -> retry al prossimo giro.
      console.warn(
        `[policy-engine] refreshDbOverrides fallito (${(err as Error).message}). ` +
          `Mantengo i flag DLP correnti (fallback YAML/ultimi noti).`
      );
    }
  }

  /**
   * True se la classificazione DLP e' disattivata da settings (`dlp_enabled=false`).
   * In tal caso il caller deve trattare ogni richiesta come tier 0.
   */
  isDlpDisabled(): boolean {
    return this.flag("dlp_enabled") === false;
  }

  decide(
    tier: SensitivityTier,
    feature: string,
    tenantFlags: Record<string, boolean> = {}
  ): RoutingDecision {
    // DLP disattivato da DB: nessuna sensibilita', si instrada sempre come tier 0.
    const effectiveTier = this.isDlpDisabled() ? (0 as SensitivityTier) : tier;

    const tierKey = `tier_${effectiveTier}` as keyof PolicyFile["routing"];
    const tierPolicy = this.policy.routing?.[tierKey];

    if (!tierPolicy) {
      return {
        providers: [],
        blocked: true,
        reason: `Nessuna policy per tier ${effectiveTier}`,
      };
    }

    // Gate cloud per-tier: la fonte di verita' del blocco cloud e' il flag DLP
    // (settings DB `dlp_allow_cloud_tierN`, fallback YAML `features.*`), NON il
    // campo struttura `tier_N.blocked`. Se il flag DLP e' definito, vince:
    //   - flag = false  -> blocca i provider cloud per questo tier
    //   - flag = true   -> consente i provider cloud, ignorando `tier_N.blocked`
    // Se il flag non e' definito (ne' DB ne' YAML), si ricade sul comportamento
    // storico basato su `tier_N.blocked`.
    const cloudGate = this.cloudGateForTier(effectiveTier);
    const blockCloudByDlp = cloudGate === false;

    if (cloudGate === undefined && tierPolicy.blocked) {
      return {
        providers: [],
        blocked: true,
        reason: `Tier ${effectiveTier} bloccato dalla policy (profilo: ${this.policy.profile})`,
      };
    }

    // Tenant override: se il tenant ha allow_cloud disabilitato, esclude provider cloud
    const tenantBlocksCloud = tenantFlags["block_cloud"] === true;
    const cloudProviders = PolicyEngine.CLOUD_PROVIDERS;

    const ordered: ProviderName[] = [
      tierPolicy.primary,
      tierPolicy.secondary,
      tierPolicy.tertiary,
      tierPolicy.fallback,
    ].filter((n): n is ProviderName => !!n);

    const excludeCloud = tenantBlocksCloud || blockCloudByDlp;
    const filtered = excludeCloud
      ? ordered.filter((p) => !cloudProviders.includes(p))
      : ordered;

    if (filtered.length === 0) {
      return {
        providers: [],
        blocked: true,
        reason: blockCloudByDlp
          ? `Tier ${effectiveTier}: provider cloud bloccati dal flag DLP (dlp_allow_cloud_tier${effectiveTier}=false) e nessun provider locale configurato`
          : `Nessun provider disponibile per tier ${effectiveTier} con i flag tenant correnti`,
      };
    }

    return { providers: filtered, blocked: false };
  }

  getProfile(): string {
    return this.policy.profile;
  }

  // Confronta il tier dichiarato col tier rilevato dal classifier.
  //
  // NON blocca piu' con 400 quando detected > claimed: il caller (mcp-core /
  // brain) NON puo' conoscere a priori il tier di sensibilita' del contenuto
  // — e' proprio il gateway a rilevarlo via classifier. Bloccare il caller per
  // non aver "indovinato" il tier rompeva ogni richiesta con PII (es. specifiche
  // applicative che menzionano email/anagrafica), perche' il default dichiarato
  // e' 0. La gestione corretta e' l'auto-elevazione: `complete`/`preview`
  // calcolano gia' `effectiveTier = max(claimed, detected)` e instradano ai
  // provider permessi per quel tier (le restrizioni di policy — es. cloud
  // tier3 — restano applicate da `decideTier`/`buildFallbackChain`). Qui ci
  // limitiamo a segnalare l'elevazione per audit, senza interrompere il flusso.
  validateTierClaim(claimed: SensitivityTier, detected: SensitivityTier): void {
    if (detected > claimed) {
      console.warn(
        `[policy-engine] tier auto-elevato: dichiarato ${claimed} -> rilevato ${detected}. ` +
          `Routing applicato sul tier effettivo ${detected} (le policy del tier restano enforced).`
      );
    }
  }
}
