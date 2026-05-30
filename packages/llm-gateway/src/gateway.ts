import { retry, ExponentialBackoff, handleAll } from "cockatiel";
import type {
  LLMProvider,
  LLMRequest,
  LLMResponse,
  LLMStreamChunk,
  ProviderName,
  ProviderStatus,
  SensitivityTier,
} from "./types.js";
import { AnthropicProvider } from "./providers/anthropic.js";
import { DeepSeekProvider } from "./providers/deepseek.js";
import { GoogleProvider } from "./providers/google.js";
import { OpenAIProvider } from "./providers/openai.js";
import { MistralProvider } from "./providers/mistral.js";
import { VLLMProvider } from "./providers/vllm-local.js";
import { ModelAliasResolver } from "./router/model-alias-resolver.js";
import { FallbackChain } from "./router/fallback-chain.js";
import { SensitivityClassifier } from "./router/sensitivity-classifier.js";
import { PolicyEngine } from "./router/policy-engine.js";
import type { SettingsDb } from "./router/policy-engine.js";
import { RateLimiter } from "./router/rate-limiter.js";
import { RedactionPipeline } from "./redaction/redaction-pipeline.js";
import { ProviderError } from "@nexus/shared";
import type { Config } from "@nexus/shared";
import {
  AuditLogger,
  buildAuditRecord,
  DLPScanner,
  AnomalyDetector,
  LangfuseTracer,
} from "@nexus/audit";

interface GatewayConfig {
  config: Config;
  aliasesFile: string;
  policyFile: string;
  /**
   * Client `postgres` per leggere i flag DLP dai settings DB (regola G).
   * Opzionale: se assente, il PolicyEngine usa solo i default YAML.
   */
  settingsDb?: SettingsDb | null;
}

export class LLMGateway {
  private providers = new Map<ProviderName, LLMProvider>();
  private statuses = new Map<string, ProviderStatus>();
  private resolver: ModelAliasResolver;
  private policy: PolicyEngine;
  private classifier: SensitivityClassifier;
  private rateLimiter: RateLimiter;
  private redaction: RedactionPipeline;
  private auditLog: AuditLogger;
  private dlp: DLPScanner;
  private anomaly: AnomalyDetector;
  private langfuse: LangfuseTracer;
  private healthCheckInterval: ReturnType<typeof setInterval> | null = null;

  constructor(private opts: GatewayConfig) {
    this.resolver = new ModelAliasResolver(opts.aliasesFile);
    this.policy = new PolicyEngine(opts.policyFile);
    this.classifier = new SensitivityClassifier(
      opts.config.redaction.presidio_grpc_url
    );
    this.rateLimiter = new RateLimiter({
      perTenant: {
        requests: opts.config.gateway.rate_limit_per_tenant_requests,
        windowMs: opts.config.gateway.rate_limit_per_tenant_window_ms,
      },
      perProvider: {
        requests: opts.config.gateway.rate_limit_per_provider_requests,
        windowMs: opts.config.gateway.rate_limit_per_provider_window_ms,
      },
    });
    this.redaction = new RedactionPipeline({
      presidioGrpcUrl: opts.config.redaction.presidio_grpc_url,
      strictMode: opts.config.redaction.strict_mode,
      ttlMs: opts.config.redaction.redaction_ttl_ms,
    });
    this.auditLog = new AuditLogger(opts.config.telemetry.log_level);
    this.dlp = new DLPScanner();
    this.anomaly = new AnomalyDetector();
    this.langfuse = new LangfuseTracer({
      host: process.env.LANGFUSE_HOST ?? "",
      secretKey: process.env.LANGFUSE_SECRET_KEY ?? "",
      enabled: !!(process.env.LANGFUSE_SECRET_KEY && process.env.LANGFUSE_HOST),
    });
    this.registerProviders();
  }

  private registerProviders() {
    const { config } = this.opts;
    const profile = config.profile;

    if (profile !== "onprem") {
      if (config.providers.anthropic?.enabled && config.providers.anthropic?.api_key) {
        this.register(
          new AnthropicProvider({
            api_key: config.providers.anthropic.api_key,
            base_url: config.providers.anthropic.base_url,
          })
        );
      }
      if (config.providers.openai?.enabled && config.providers.openai?.api_key) {
        this.register(
          new OpenAIProvider({
            api_key: config.providers.openai.api_key,
            base_url: config.providers.openai.base_url,
          })
        );
      }
      if (config.providers.mistral?.enabled && config.providers.mistral?.api_key) {
        this.register(
          new MistralProvider({
            api_key: config.providers.mistral.api_key,
          })
        );
      }
      const deepseekKey = (config.providers as any).deepseek?.api_key ?? process.env.DEEPSEEK_API_KEY;
      if (deepseekKey && process.env.DEEPSEEK_PROVIDER_ENABLED !== "false") {
        this.register(new DeepSeekProvider({ api_key: deepseekKey }));
      }
      const googleKey = (config.providers as any).google?.api_key ?? process.env.GOOGLE_API_KEY;
      if (googleKey && process.env.GOOGLE_PROVIDER_ENABLED !== "false") {
        this.register(new GoogleProvider({ api_key: googleKey }));
      }
    }

    if ((profile === "hybrid" || profile === "onprem") && config.vllm) {
      this.register(
        new VLLMProvider({
          base_url: config.vllm.base_url,
          api_key: config.vllm.api_key,
          max_context_tokens: config.vllm.max_context_tokens,
        })
      );
    }
  }

  private register(provider: LLMProvider) {
    this.providers.set(provider.name as ProviderName, provider);
    this.statuses.set(provider.name, {
      name: provider.name,
      healthy: true,
      last_check: new Date(),
    });
  }

  /**
   * Provider considerati "locali" (on-premise, massima sicurezza per qualsiasi tier).
   * Non inviano dati a servizi cloud esterni — hanno la precedenza assoluta nel fallback privacy.
   */
  private static readonly LOCAL_PROVIDERS: ProviderName[] = ["vllm"];

  /**
   * Quando la policy blocca una richiesta per motivi di privacy, cerca il provider
   * più adatto tra quelli registrati, con questo ordine di priorità:
   *
   * 1. Provider locali (on-premise) compatibili col tier — massima sicurezza
   * 2. Provider cloud compatibili col tier — se il blocco deriva solo dalla policy
   *    (es. `blocked: true`) e non da un flag esplicito `block_cloud` del tenant
   *
   * Restituisce `undefined` se nessun provider può gestire il tier richiesto.
   */
  private findCompatibleFallback(
    tier: SensitivityTier,
    tenantBlocksCloud: boolean
  ): LLMProvider | undefined {
    // 1. Provider locali compatibili col tier (priorità assoluta)
    for (const name of LLMGateway.LOCAL_PROVIDERS) {
      const p = this.providers.get(name);
      if (p && (p.tier_compatibility as number[]).includes(tier)) return p;
    }

    // 2. Se il tenant ha blocco cloud esplicito, non andare oltre
    if (tenantBlocksCloud) return undefined;

    // 3. Qualsiasi provider cloud registrato compatibile col tier
    //    (ordered map iteration mantiene l'ordine di registrazione)
    for (const [, p] of this.providers) {
      if ((p.tier_compatibility as number[]).includes(tier)) return p;
    }

    return undefined;
  }

  private buildFallbackChain(
    tier: SensitivityTier,
    tenantFlags: Record<string, boolean> = {}
  ): { chain: FallbackChain; primaryName: ProviderName; privacyRerouted?: { provider: string; blocked_tier: number; reason: string } } {
    const decision = this.policy.decide(tier, "", tenantFlags);

    if (decision.blocked) {
      const tenantBlocksCloud = tenantFlags["block_cloud"] === true;
      const fallback = this.findCompatibleFallback(tier, tenantBlocksCloud);

      if (fallback) {
        const isLocal = LLMGateway.LOCAL_PROVIDERS.includes(fallback.name as ProviderName);
        const privacyRerouted = {
          provider: fallback.name,
          blocked_tier: tier,
          reason: isLocal
            ? `Il contenuto è stato classificato come sensibile (tier ${tier}). ` +
              `La richiesta è stata instradata automaticamente sul modello locale "${fallback.name}" ` +
              `per garantire che i dati non vengano inviati a provider cloud esterni.`
            : `Il contenuto è stato classificato come sensibile (tier ${tier}). ` +
              `Routing di default bloccato dalla policy; la richiesta è stata instradata ` +
              `automaticamente su "${fallback.name}" che supporta questo livello di sensibilità.`,
        };
        return {
          chain: new FallbackChain([fallback], this.statuses),
          primaryName: fallback.name as ProviderName,
          privacyRerouted,
        };
      }

      // Nessun provider disponibile per questo tier: errore user-friendly
      throw new ProviderError(
        `Contenuto sensibile rilevato (livello ${tier}). ` +
          `La richiesta è stata bloccata: nessun provider abilitato supporta questo livello di sensibilità. ` +
          `Puoi: (1) riformulare la richiesta rimuovendo i dati sensibili, ` +
          `oppure (2) configurare un provider on-premise (vLLM) nelle impostazioni.`,
        "gateway",
        403,
        { tier, privacy_blocked: true }
      );
    }

    const resolved = decision.providers
      .map((name) => this.providers.get(name))
      .filter((p): p is LLMProvider => !!p);

    if (resolved.length === 0) {
      throw new ProviderError(`Nessun provider registrato per tier ${tier}`, "gateway", 503);
    }

    return {
      chain: new FallbackChain(resolved, this.statuses),
      primaryName: decision.providers[0],
    };
  }

  /**
   * Preview “deterministico” del routing: ritorna provider primario, modello risolto e tier effettivo.
   * Serve per calcoli di quota/costo *prima* della chiamata al provider (calcolo perfetto).
   *
   * Nota: non applica rate limit per evitare doppio conteggio; il rate limit resta in `complete/stream`.
   */
  async preview(req: LLMRequest): Promise<{ primaryName: ProviderName; resolvedModel: string; effectiveTier: SensitivityTier }> {
    // Refresh lazy dei flag DLP dal DB (cache TTL 60s interna)
    await this.refreshPolicyOverrides(this.opts.settingsDb);

    // Classificazione sensitivity (stessa logica di `complete`)
    const classification = await this.classifier.classify(req.messages);
    this.policy.validateTierClaim(req.metadata.sensitivity_tier, classification.tier);
    const effectiveTier = Math.max(req.metadata.sensitivity_tier, classification.tier) as SensitivityTier;

    // Policy routing (stesso meccanismo)
    const { primaryName, privacyRerouted } = this.buildFallbackChain(effectiveTier);

    // Alias resolution del modello per il provider primario
    let resolvedModel = req.model;
    try {
      resolvedModel = this.resolver.resolve(req.model, primaryName as any, effectiveTier);
    } catch {
      // fallback: usa req.model
      resolvedModel = req.model;
    }

    // Se privacy-rerouted verso provider locale, il primaryName è già quello; resolvedModel coerente.
    void privacyRerouted; // solo per chiarezza (nessuna side-effect qui)
    return { primaryName, resolvedModel, effectiveTier };
  }

  async complete(req: LLMRequest): Promise<LLMResponse> {
    // 0. Refresh lazy dei flag DLP dal DB (cache TTL 60s interna)
    await this.refreshPolicyOverrides(this.opts.settingsDb);

    // 1. Rate limit
    this.rateLimiter.checkTenant(req.metadata.tenant_id);

    // 2. Classificazione sensitivity (arricchisce, non sovrascrive il tier dichiarato)
    const classification = await this.classifier.classify(req.messages);

    // 3. Valida che il tier dichiarato non sia inferiore al rilevato
    this.policy.validateTierClaim(req.metadata.sensitivity_tier, classification.tier);

    const effectiveTier = Math.max(
      req.metadata.sensitivity_tier,
      classification.tier
    ) as SensitivityTier;

    // 4. Policy routing
    const { chain, primaryName, privacyRerouted } = this.buildFallbackChain(effectiveTier);

    // Rate limit per provider primario
    this.rateLimiter.checkProvider(primaryName);

    // 5. Risoluzione alias modello per ogni provider della catena (fallback-safe)
    // Miglioria token/latency: se un provider NON è compatibile col modello richiesto/alias,
    // lo escludiamo dalla chain invece di provarlo e fallire (riduce retry e 503 generici).
    const modelPerProvider = new Map<string, string>();
    const providersToResolve = privacyRerouted
      ? [primaryName]
      : this.policy.decide(effectiveTier, "").providers;

    // Risolvi modello per ciascun provider; se fallisce, segnala incompatibilità.
    const incompatible = new Set<string>();
    for (const pName of providersToResolve) {
      try {
        const m = this.resolver.resolve(req.model, pName as any, effectiveTier);
        modelPerProvider.set(pName, m);
      } catch {
        // Se il modello è prefissato (es. "anthropic/..") e non c'è fallback, questo provider è incompatibile
        // e va escluso dalla chain per questa richiesta.
        incompatible.add(pName);
      }
    }

    // Applica filtro chain quando abbiamo incompatibilità (solo path non-privacy-rerouted).
    if (!privacyRerouted && incompatible.size > 0) {
      chain.filterInPlace((p) => !incompatible.has(p.name));
    }
    chain.modelPerProvider = modelPerProvider;
    const resolvedModel = modelPerProvider.get(primaryName) ?? req.model;

    // 6. Pre-flight redaction (solo se il provider è cloud)
    const isCloud = primaryName !== "vllm";
    const redactedReq = isCloud && this.opts.config.redaction.enabled
      ? await this.redaction.redact({ ...req, metadata: { ...req.metadata, sensitivity_tier: effectiveTier } })
      : null;

    const finalReq = redactedReq
      ? { ...req, messages: redactedReq.messages, model: resolvedModel, metadata: { ...req.metadata, sensitivity_tier: effectiveTier } }
      : { ...req, model: resolvedModel, metadata: { ...req.metadata, sensitivity_tier: effectiveTier } };

    // 7. Esecuzione con retry
    const retryPolicy = retry(handleAll, {
      maxAttempts: 3,
      backoff: new ExponentialBackoff(),
    });

    const response = await retryPolicy.execute(() => chain.complete(finalReq));

    // 8. Post-flight rehydration
    const rehydrated = redactedReq ? this.redaction.rehydrate(response, redactedReq.map) : response;
    // Propaga il flag privacy_rerouted se la richiesta è stata re-instradata su provider locale
    const finalResponse = privacyRerouted
      ? { ...rehydrated, privacy_rerouted: privacyRerouted }
      : rehydrated;

    // 9. DLP post-response: blocca se il modello ha rigurgitato segreti tier-3
    const dlpResult = this.dlp.assertSafeResponse(req.metadata.request_id, finalResponse.content);

    // 10. Injection scan sul prompt originale (prima della redaction)
    const messagesText = req.messages
      .map((m) => (typeof m.content === "string" ? m.content : JSON.stringify(m.content)))
      .join("\n");
    const injectionCheck = this.dlp.scanForInjection(messagesText);

    // 11. Audit record strutturato (solo hash, mai payload in chiaro)
    const record = buildAuditRecord({
      req,
      resp: finalResponse,
      redactionApplied: !!redactedReq,
      dlpBlocked: false,
      dlpPatterns: dlpResult.patterns,
    });
    this.auditLog.logAuditRecord(record);

    // 12. Anomaly detection
    const anomalies = this.anomaly.analyze({
      tenant_id: req.metadata.tenant_id,
      request_id: req.metadata.request_id,
      input_tokens: finalResponse.usage.input_tokens,
      output_tokens: finalResponse.usage.output_tokens,
      sensitivity_tier: effectiveTier,
      finish_reason: finalResponse.finish_reason,
      injection_detected: injectionCheck.detected,
    });
    for (const a of anomalies) {
      this.auditLog.logAnomaly(req.metadata.request_id, req.metadata.tenant_id, a.type, a.detail);
    }

    // 13. Langfuse trace (fire-and-forget — errori non bloccano la response)
    void this.langfuse.traceCall({ req, resp: finalResponse, record });

    return finalResponse;
  }

  async *stream(req: LLMRequest): AsyncIterable<LLMStreamChunk> {
    // Refresh lazy dei flag DLP dal DB (cache TTL 60s interna)
    await this.refreshPolicyOverrides(this.opts.settingsDb);

    this.rateLimiter.checkTenant(req.metadata.tenant_id);

    // Classificazione sincrona per lo streaming (bassa latency path)
    const classification = this.classifier.classifySync(req.messages);
    this.policy.validateTierClaim(req.metadata.sensitivity_tier, classification.tier);

    const effectiveTier = Math.max(
      req.metadata.sensitivity_tier,
      classification.tier
    ) as SensitivityTier;

    const decision = this.policy.decide(effectiveTier, "");
    if (decision.blocked) {
      // Stesso meccanismo di auto-fallback usato nel path non-streaming
      const tenantBlocksCloud = false; // streaming non ha tenantFlags disponibili qui
      const fallback = this.findCompatibleFallback(effectiveTier, tenantBlocksCloud);
      if (fallback) {
        const resolvedModel = (() => {
          try { return this.resolver.resolve(req.model, fallback.name as any, effectiveTier); }
          catch { return req.model; }
        })();
        yield* fallback.stream({ ...req, model: resolvedModel });
        return;
      }
      throw new ProviderError(
        `Contenuto sensibile rilevato (livello ${effectiveTier}). ` +
          `Nessun provider abilitato supporta questo livello di sensibilità. ` +
          `Riformula il messaggio rimuovendo i dati sensibili ` +
          `oppure configura un provider on-premise nelle impostazioni.`,
        "gateway",
        403,
        { tier: effectiveTier, privacy_blocked: true }
      );
    }

    const primaryName = decision.providers[0];
    const provider = this.providers.get(primaryName);
    if (!provider) {
      throw new ProviderError(`Provider "${primaryName}" non disponibile`, "gateway", 503);
    }

    this.rateLimiter.checkProvider(primaryName);
    const resolvedModel = (() => {
      try {
        return this.resolver.resolve(req.model, primaryName, effectiveTier);
      } catch {
        return req.model;
      }
    })();

    for await (const chunk of provider.stream({ ...req, model: resolvedModel })) {
      // Arricchisce i chunk con provider/modello per telemetria lato API gateway
      // (campi opzionali: retro-compatibili per i client).
      if (chunk.usage || chunk.finish_reason) {
        yield { ...(chunk as any), provider_used: primaryName, model_used: resolvedModel };
      } else {
        yield chunk;
      }
    }
  }

  startHealthChecks(intervalMs = 60_000) {
    this.healthCheckInterval = setInterval(async () => {
      for (const [name, provider] of this.providers) {
        const healthy = await provider.healthcheck().catch(() => false);
        const current = this.statuses.get(name);
        if (current) {
          // Legge billing_error se il provider lo espone (es. AnthropicProvider).
          // Il campo persiste finché il provider non viene reinizializzato o i crediti
          // non vengono ripristinati (il healthcheck ritorna true di nuovo).
          const billingError = (provider as { billingError?: string | null }).billingError ?? undefined;
          this.statuses.set(name, {
            ...current,
            healthy: healthy && !billingError,
            last_error: billingError ?? current.last_error,
            billing_error: billingError || undefined,
            last_check: new Date(),
          });
        }
      }
    }, intervalMs);
    this.healthCheckInterval.unref?.();
  }

  /**
   * Ricarica i flag DLP per-tier (`dlp_allow_cloud_tier2/3`, `dlp_enabled`) dai
   * settings DB nel PolicyEngine. Cache TTL 60s gestita internamente: chiamabile
   * a ogni richiesta senza costo DB ripetuto. Fallback graceful se DB down.
   *
   * Il gateway non possiede una connessione DB propria: il client `postgres`
   * viene iniettato dal chiamante (nexus-gateway), coerentemente con l'iniezione
   * di config/aliasesFile/policyFile.
   */
  async refreshPolicyOverrides(db: SettingsDb | null | undefined, force = false): Promise<void> {
    await this.policy.refreshDbOverrides(db, force);
  }

  stopHealthChecks() {
    if (this.healthCheckInterval) {
      clearInterval(this.healthCheckInterval);
      this.healthCheckInterval = null;
    }
  }

  getProviderStatuses(): ProviderStatus[] {
    return [...this.statuses.values()];
  }

  getRegisteredProviders(): ProviderName[] {
    return [...this.providers.keys()];
  }
}
