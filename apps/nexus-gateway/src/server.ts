import Fastify from "fastify";
import { resolve, dirname } from "path";
import { fileURLToPath } from "url";
import postgres from "postgres";
import { LLMGateway } from "@nexus/llm-gateway";
import { loadConfig, JWTService } from "@nexus/shared";
import type { LLMRequest } from "@nexus/shared";

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
  const sql = postgres(dbUrl, { max: 1, idle_timeout: 5 });
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
  } finally {
    await sql.end();
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

app.get("/health", async () => ({
  status: "ok",
  profile: config.profile,
  providers: gateway.getProviderStatuses().map((p) => ({
    name: p.name,
    healthy: p.healthy,
    last_check: p.last_check,
  })),
}));

app.get("/providers", async () => ({
  providers: gateway.getProviderStatuses(),
}));

app.post("/v1/complete", async (req, reply) => {
  const body = req.body as LLMRequest;
  if (!body?.messages?.length) {
    return reply.code(400).send({ error: "messages required" });
  }
  try {
    const response = await gateway.complete(body);
    return response;
  } catch (err: unknown) {
    const e = err as { code?: string; message?: string; status?: number };
    const status = e.status ?? (e.code === "TIER_BLOCKED" || e.code === "DLP_BLOCKED" ? 403 : 500);
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
    for await (const chunk of gateway.stream(body)) {
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
