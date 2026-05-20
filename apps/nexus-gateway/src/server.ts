import Fastify from "fastify";
import { resolve, dirname } from "path";
import { fileURLToPath } from "url";
import postgres from "postgres";
import { LLMGateway } from "@nexus/llm-gateway";
import { loadConfig, JWTService } from "@nexus/shared";
import type { LLMRequest } from "@nexus/shared";
import { NexusError } from "@nexus/shared";

// Repo root = apps/nexus-gateway/src/../../.. → D:/Sviluppo/IDEAI
const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../../..");

const PORT = Number(process.env.NEXUS_GATEWAY_PORT ?? 4060);
const ALIASES_FILE = resolve(REPO_ROOT, process.env.NEXUS_MODEL_ALIASES_FILE ?? "config/model-aliases.yaml");
const POLICY_FILE  = resolve(REPO_ROOT, process.env.NEXUS_LLM_POLICY_FILE  ?? "config/policies/default.yaml");
const JWT_SECRET   = process.env.JWT_SECRET ?? "";

const DB_KEY_MAP: Record<string, string> = {
  // API key provider
  openai_api_key:                    "OPENAI_API_KEY",
  anthropic_api_key:                 "ANTHROPIC_API_KEY",
  mistral_api_key:                   "MISTRAL_API_KEY",
  deepseek_api_key:                  "DEEPSEEK_API_KEY",
  google_api_key:                    "GOOGLE_API_KEY",
  // Provider enabled/disabled flags
  openai_enabled:                    "OPENAI_PROVIDER_ENABLED",
  anthropic_enabled:                 "ANTHROPIC_PROVIDER_ENABLED",
  mistral_enabled:                   "MISTRAL_PROVIDER_ENABLED",
  deepseek_enabled:                  "DEEPSEEK_PROVIDER_ENABLED",
  google_enabled:                    "GOOGLE_PROVIDER_ENABLED",
  // Gateway config — sovrascrivono i default di GatewayConfigSchema
  rate_limit_per_tenant_requests:    "RATE_LIMIT_PER_TENANT_REQUESTS",
  rate_limit_per_tenant_window_ms:   "RATE_LIMIT_PER_TENANT_WINDOW_MS",
  rate_limit_per_provider_requests:  "RATE_LIMIT_PER_PROVIDER_REQUESTS",
  rate_limit_per_provider_window_ms: "RATE_LIMIT_PER_PROVIDER_WINDOW_MS",
  health_check_interval_ms:          "HEALTH_CHECK_INTERVAL_MS",
  default_max_tokens:                "DEFAULT_MAX_TOKENS",
};

async function loadApiKeysFromDb(): Promise<void> {
  const dbUrl = process.env.POSTGRES_URL ?? process.env.DATABASE_URL;
  if (!dbUrl) return;
  // Retry con backoff esponenziale per resilienza al boot: Postgres potrebbe
  // ancora essere in fase di startup (container Docker non pronto, race con
  // systemd) e dare ECONNRESET. Senza retry, il gateway moriva e systemd lo
  // riavviava in loop. Con retry, attende fino a ~30s che il DB sia pronto.
  const MAX_ATTEMPTS = 5;
  for (let attempt = 1; attempt <= MAX_ATTEMPTS; attempt++) {
    const sql = postgres(dbUrl, { max: 1, idle_timeout: 5, connect_timeout: 5 });
    try {
      const rows = await sql<{ key: string; value: string }[]>`
        SELECT key, value FROM settings
        WHERE key = ANY(${Object.keys(DB_KEY_MAP)})
          AND value IS NOT NULL AND value <> ''
      `;
      for (const { key, value } of rows) {
        const envKey = DB_KEY_MAP[key];
        if (envKey) process.env[envKey] = value;
      }
      await sql.end();
      return; // successo
    } catch (err) {
      await sql.end().catch(() => {});
      if (attempt === MAX_ATTEMPTS) {
        // Non rilanciare: meglio partire senza chiavi e farle ricaricare dopo
        // (via /admin/reload o restart) piuttosto che crashare e impedire al
        // gateway di accettare anche request che non richiedono LLM (health).
        console.error(`[gateway] loadApiKeysFromDb fallito dopo ${MAX_ATTEMPTS} tentativi: ${(err as Error).message}. Provider keys NON caricate dal DB.`);
        return;
      }
      const wait = Math.min(2000 * Math.pow(2, attempt - 1), 8000);
      console.warn(`[gateway] loadApiKeysFromDb tentativo ${attempt}/${MAX_ATTEMPTS} fallito (${(err as Error).message}), retry tra ${wait}ms`);
      await new Promise((r) => setTimeout(r, wait));
    }
  }
}

const app = Fastify({
  logger: {
    level: process.env.LOG_LEVEL ?? "info",
    transport: process.env.NODE_ENV !== "production"
      ? { target: "pino-pretty", options: { colorize: true } }
      : undefined,
  },
});

// ── Bootstrap ──────────────────────────────────────────────────────────────────
process.env.NEXUS_LLM_POLICY_FILE    = POLICY_FILE;
process.env.NEXUS_MODEL_ALIASES_FILE = ALIASES_FILE;

// Carica le API key dal DB prima di inizializzare i provider
await loadApiKeysFromDb();

let config = await loadConfig();
let gateway = new LLMGateway({ config, aliasesFile: ALIASES_FILE, policyFile: POLICY_FILE });
gateway.startHealthChecks(config.gateway.health_check_interval_ms);

// ── Billing / Accounting (DB ledger) ───────────────────────────────────────────
const BILLING_DB_URL = config.database.url ?? process.env.POSTGRES_URL ?? process.env.DATABASE_URL;
const billingSql = BILLING_DB_URL ? postgres(BILLING_DB_URL, { max: 5, idle_timeout: 10 }) : null;

type PriceSnapshot = {
  input_cost_per_million_tokens: number;
  output_cost_per_million_tokens: number;
  currency: string;
};

async function getPlatformCurrency(): Promise<string> {
  if (!billingSql) return "EUR";
  const rows = await billingSql<{ value: string }[]>`
    SELECT value FROM settings WHERE key = 'billing_base_currency'
  `;
  const v = rows?.[0]?.value ?? "EUR";
  return String(v).trim().toUpperCase() || "EUR";
}

async function resolveActivePrice(provider: string, model: string): Promise<PriceSnapshot | null> {
  if (!billingSql) return null;
  const currency = await getPlatformCurrency();
  const rows = await billingSql<PriceSnapshot[]>`
    SELECT
      input_cost_per_million_tokens::float8 as input_cost_per_million_tokens,
      output_cost_per_million_tokens::float8 as output_cost_per_million_tokens,
      currency
    FROM ai_price_catalog
    WHERE provider = ${provider}
      AND model = ${model}
      AND currency = ${currency}
      AND is_enabled = TRUE
      AND effective_from <= NOW()
      AND (effective_to IS NULL OR effective_to > NOW())
    ORDER BY effective_from DESC
    LIMIT 1
  `;
  return rows?.[0] ?? null;
}

function calculateCost(price: PriceSnapshot, promptTokens: number, completionTokens: number) {
  const inputCost =
    (Math.max(0, promptTokens) / 1_000_000.0) * Number(price.input_cost_per_million_tokens);
  const outputCost =
    (Math.max(0, completionTokens) / 1_000_000.0) * Number(price.output_cost_per_million_tokens);
  return { inputCost, outputCost, totalCost: inputCost + outputCost };
}

async function enforceQuota(req: LLMRequest, provider: string, model: string): Promise<void> {
  if (!billingSql) return;

  // Convenzione attuale: tenant_id = project_id (UUID) nel mondo Nexus.
  const projectId = req.metadata?.tenant_id;
  const userId = req.metadata?.user_id;
  if (!projectId || !userId) return;

  const currency = await getPlatformCurrency();

  // Stima “prima della chiamata”: token input ~ char/4, token output ~ max_tokens (se presente).
  const estimatedPromptTokens = Math.ceil(
    (req.messages ?? []).reduce((acc, m: any) => acc + (typeof m?.content === "string" ? m.content.length : 0), 0) / 4
  );
  const estimatedCompletionTokens = Number((req as any)?.max_tokens ?? 0) || 0;
  const estimatedTotalTokens = estimatedPromptTokens + estimatedCompletionTokens;

  const price = await resolveActivePrice(provider, model);
  const estimatedCosts = price
    ? calculateCost(price, estimatedPromptTokens, estimatedCompletionTokens)
    : { inputCost: 0, outputCost: 0, totalCost: 0 };

  const quotas = await billingSql<{
    scope_type: "user" | "project" | "user_project";
    token_limit: number | null;
    cost_limit: number | null;
    valid_from: string;
    valid_to: string;
  }[]>`
    SELECT scope_type, token_limit::bigint as token_limit, cost_limit::float8 as cost_limit, valid_from, valid_to
    FROM ai_quota_policies
    WHERE is_enabled = TRUE
      AND valid_from <= NOW()
      AND valid_to > NOW()
      AND (
        (scope_type = 'user' AND user_id = ${userId}::uuid) OR
        (scope_type = 'project' AND project_id = ${projectId}::uuid) OR
        (scope_type = 'user_project' AND user_id = ${userId}::uuid AND project_id = ${projectId}::uuid)
      )
    ORDER BY scope_type ASC
  `;

  if (!quotas?.length) return;

  // Uso corrente calcolato su ledger (reserved/finalized) nel periodo del vincolo
  for (const q of quotas) {
    const usage = await billingSql<{ tokens: number; cost: number }[]>`
      SELECT
        COALESCE(SUM(total_tokens), 0)::bigint as tokens,
        COALESCE(SUM(total_cost), 0)::float8 as cost
      FROM ai_usage_ledger
      WHERE status IN ('reserved', 'finalized')
        AND created_at >= ${q.valid_from}::timestamptz
        AND created_at <  ${q.valid_to}::timestamptz
        AND (
          (${q.scope_type} = 'user'       AND user_id = ${userId}::uuid) OR
          (${q.scope_type} = 'project'    AND project_id = ${projectId}::uuid) OR
          (${q.scope_type} = 'user_project' AND user_id = ${userId}::uuid AND project_id = ${projectId}::uuid)
        )
    `;
    const tokens = Number(usage?.[0]?.tokens ?? 0);
    const cost = Number(usage?.[0]?.cost ?? 0);

    if (q.token_limit != null && (tokens + estimatedTotalTokens) > Number(q.token_limit)) {
      throw new NexusError(
        "QUOTA_EXCEEDED",
        `Quota token superata (${tokens}+${estimatedTotalTokens}/${q.token_limit})`,
        403,
        { scope: q.scope_type, currency, provider, model }
      );
    }
    if (q.cost_limit != null && (cost + estimatedCosts.totalCost) > Number(q.cost_limit)) {
      throw new NexusError(
        "QUOTA_EXCEEDED",
        `Quota costo superata (${cost}+${estimatedCosts.totalCost}/${q.cost_limit} ${currency})`,
        403,
        { scope: q.scope_type, currency, provider, model }
      );
    }
  }
}

async function recordUsageToLedger(req: LLMRequest, resp: { provider_used: string; model_used: string; usage?: { input_tokens: number; output_tokens: number } }) {
  if (!billingSql) return;
  const projectId = req.metadata?.tenant_id;
  const userId = req.metadata?.user_id;
  if (!projectId || !userId || !resp.usage) return;

  const provider = resp.provider_used;
  const model = resp.model_used;
  const promptTokens = Number(resp.usage.input_tokens ?? 0);
  const completionTokens = Number(resp.usage.output_tokens ?? 0);
  const totalTokens = promptTokens + completionTokens;

  const price = await resolveActivePrice(provider, model);
  const currency = (price?.currency ?? (await getPlatformCurrency())).toString().trim().toUpperCase();
  const costs = price ? calculateCost(price, promptTokens, completionTokens) : { inputCost: 0, outputCost: 0, totalCost: 0 };

  await billingSql`
    INSERT INTO ai_usage_ledger (
      user_id, project_id, provider, model,
      prompt_tokens, completion_tokens, total_tokens,
      input_cost, output_cost, total_cost,
      currency, status, details
    ) VALUES (
      ${userId}::uuid, ${projectId}::uuid, ${provider}, ${model},
      ${promptTokens}, ${completionTokens}, ${totalTokens},
      ${costs.inputCost}, ${costs.outputCost}, ${costs.totalCost},
      ${currency}, 'finalized',
      ${billingSql.json({
        request_id: req.metadata?.request_id,
        feature: req.metadata?.feature,
        price_missing: !price,
      })}
    )
  `;
}

async function reloadGateway(): Promise<{ reloaded: boolean; providers: string[] }> {
  await loadApiKeysFromDb();
  config = await loadConfig();
  gateway.stopHealthChecks();
  gateway = new LLMGateway({ config, aliasesFile: ALIASES_FILE, policyFile: POLICY_FILE });
  gateway.startHealthChecks(config.gateway.health_check_interval_ms);
  const providers = gateway.getProviderStatuses().map(p => p.name);
  return { reloaded: true, providers };
}

const jwtService = JWT_SECRET.length >= 32 ? new JWTService(JWT_SECRET) : null;
if (!jwtService) {
  app.log.warn("JWT_SECRET troppo corto o assente — autenticazione DISABILITATA (dev only)");
}

// ── Auth hook ──────────────────────────────────────────────────────────────────
app.addHook("preHandler", async (req, reply) => {
  if (req.url === "/health" || req.url === "/providers") return;
  if (!jwtService) return; // dev mode senza JWT

  const auth = req.headers["authorization"];
  if (!auth?.startsWith("Bearer ")) {
    return reply.code(401).send({ error: "Unauthorized", code: "MISSING_TOKEN" });
  }
  const token = auth.slice(7);
  // Bypass JWT per chiamate interne (mcp-core → nexus-gateway)
  const serviceToken = process.env.NEXUS_GATEWAY_SERVICE_TOKEN ?? "dev-internal-token";
  if (token === serviceToken) return;
  try {
    await jwtService.verify(token);
  } catch {
    return reply.code(401).send({ error: "Unauthorized", code: "INVALID_TOKEN" });
  }
});

// ── Routes ─────────────────────────────────────────────────────────────────────

// ── Stato provider: delega a mcp-core ─────────────────────────────────────
//
// Il gateway TypeScript NON tiene piu' una cache locale dello stato dei
// provider. La fonte di verita' canonica e' `nexus_provider_health_history`
// scritto da `provider_health_probe.rs` (Rust) che gira ogni 5 min, persiste
// in DB e ha auto-recovery cooldown + outage detection.
//
// Motivazione: in passato il gateway aveva una sua `Map<provider, healthy>`
// in memoria popolata da `gateway.startHealthChecks()` con cadenza 60s. Se
// `loadApiKeysFromDb` falliva al boot (es. ECONNRESET su Postgres), tutti
// i provider risultavano unhealthy nella memoria del gateway e l'utente
// vedeva tutti i LED rossi nella UI ANCHE quando il probe Rust diceva
// healthy. Due fonti di verita' = inconsistenza garantita.
//
// Ora: `/health` e `/providers` proxano a mcp-core; se irraggiungibile,
// fallback a un payload minimale (cosi' i monitoring esterni che pollano
// `/health` continuano a vedere status=ok).
const MCP_CORE_URL = process.env.MCP_CORE_URL ?? "http://localhost:4000";

async function fetchProvidersFromMcpCore(): Promise<Array<Record<string, unknown>> | null> {
  try {
    const res = await fetch(`${MCP_CORE_URL}/api/internal/providers/status`, {
      method: "GET",
      signal: AbortSignal.timeout(3000),
    });
    if (!res.ok) return null;
    const data = (await res.json()) as { providers?: Array<Record<string, unknown>> };
    return Array.isArray(data?.providers) ? data.providers : null;
  } catch {
    return null;
  }
}

app.get("/health", async () => {
  const fromMcp = await fetchProvidersFromMcpCore();
  return {
    status: "ok",
    profile: config.profile,
    providers: fromMcp ?? gateway.getProviderStatuses().map((p) => ({
      name: p.name,
      healthy: p.healthy,
      last_check: p.last_check,
    })),
  };
});

app.get("/providers", async () => {
  const fromMcp = await fetchProvidersFromMcpCore();
  return {
    providers: fromMcp ?? gateway.getProviderStatuses(),
  };
});

app.post("/v1/complete", async (req, reply) => {
  const body = req.body as LLMRequest;
  if (!body?.messages?.length) {
    return reply.code(400).send({ error: "messages required" });
  }
  try {
    // Guardrail “perfetto”: risolve provider/modello effettivi PRIMA della chiamata.
    const preview = await gateway.preview(body);
    await enforceQuota(body, preview.primaryName, preview.resolvedModel);
    const response = await gateway.complete(body);
    // Telemetria: scrive sempre su ledger (se DB configurato).
    await recordUsageToLedger(body, {
      provider_used: response.provider_used,
      model_used: response.model_used,
      usage: response.usage,
    });
    return response;
  } catch (err: unknown) {
    const e = err as { code?: string; message?: string; status?: number; statusCode?: number };
    const status =
      (e.statusCode ?? e.status) ??
      (e.code === "TIER_BLOCKED" || e.code === "DLP_BLOCKED" || e.code === "QUOTA_EXCEEDED" ? 403 : 500);
    return reply.code(status).send({ error: e.message, code: e.code });
  }
});

app.post("/v1/stream", async (req, reply) => {
  const body = req.body as LLMRequest;
  if (!body?.messages?.length) {
    return reply.code(400).send({ error: "messages required" });
  }

  reply.raw.writeHead(200, {
    "Content-Type": "text/event-stream",
    "Cache-Control": "no-cache",
    "Connection": "keep-alive",
  });

  try {
    // Guardrail “perfetto” anche per lo streaming
    const preview = await gateway.preview(body);
    await enforceQuota(body, preview.primaryName, preview.resolvedModel);
    for await (const chunk of gateway.stream(body)) {
      // Se il chunk finale include usage + provider/model, scrivi ledger.
      if ((chunk as any)?.usage && (chunk as any)?.provider_used && (chunk as any)?.model_used) {
        void recordUsageToLedger(body, {
          provider_used: (chunk as any).provider_used,
          model_used: (chunk as any).model_used,
          usage: (chunk as any).usage,
        }).catch(() => undefined);
      }
      reply.raw.write(`data: ${JSON.stringify(chunk)}\n\n`);
    }
    reply.raw.write("data: [DONE]\n\n");
  } catch (err: unknown) {
    const e = err as { message?: string };
    reply.raw.write(`data: ${JSON.stringify({ error: e.message })}\n\n`);
  } finally {
    reply.raw.end();
  }
});

// ── Admin ─────────────────────────────────────────────────────────────────────

app.post("/admin/reload", async (req, reply) => {
  // Requires service token or admin JWT
  const auth = (req.headers["authorization"] ?? "") as string;
  const serviceToken = process.env.NEXUS_GATEWAY_SERVICE_TOKEN ?? "dev-internal-token";
  const token = auth.startsWith("Bearer ") ? auth.slice(7) : "";
  if (jwtService && token !== serviceToken) {
    try { await jwtService.verify(token); } catch {
      return reply.code(401).send({ error: "Unauthorized" });
    }
  }
  try {
    const result = await reloadGateway();
    app.log.info({ providers: result.providers }, "Gateway ricaricato dal DB");
    return result;
  } catch (err: unknown) {
    const e = err as { message?: string };
    return reply.code(500).send({ error: e.message ?? "reload failed" });
  }
});

// ── Start ──────────────────────────────────────────────────────────────────────
try {
  await app.listen({ port: PORT, host: "0.0.0.0" });
  app.log.info(`Nexus Gateway in ascolto su :${PORT} [profilo: ${config.profile}]`);
} catch (err) {
  app.log.error(err);
  process.exit(1);
}
