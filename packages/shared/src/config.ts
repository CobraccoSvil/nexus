import { z } from "zod";
import { readFileSync } from "fs";
import { parse as parseYaml } from "yaml";
import { resolve } from "path";

const ProfileSchema = z.enum(["cloud", "hybrid", "onprem"]).default("cloud");

const ProviderConfigSchema = z.object({
  enabled: z.boolean().default(true),
  api_key: z.string().optional(),
  base_url: z.string().url().optional(),
  timeout_ms: z.number().default(30000),
});

const VLLMConfigSchema = z.object({
  base_url: z.string().url().default("http://localhost:8000/v1"),
  api_key: z.string().default(""),
  model_name: z.string().default("Qwen/Qwen2.5-Coder-32B-Instruct"),
  max_context_tokens: z.number().default(32768),
});

const RedactionConfigSchema = z.object({
  enabled: z.boolean().default(true),
  strict_mode: z.boolean().default(false),
  presidio_grpc_url: z.string().default(
    process.env.PRESIDIO_GRPC_URL ?? "localhost:50052"
  ),
  redaction_ttl_ms: z.number().default(300000),
});

const TelemetryConfigSchema = z.object({
  enabled: z.boolean().default(true),
  otlp_endpoint: z.string().default(
    process.env.OTLP_ENDPOINT ?? "http://localhost:4318"
  ),
  log_level: z.enum(["debug", "info", "warn", "error"]).default("info"),
  service_name: z.string().default("nexus-llm-gateway"),
});

const DatabaseConfigSchema = z.object({
  url: z.string().url().optional(),
  pool_size: z.number().default(20),
  ssl: z.boolean().default(false),
});

const RedisConfigSchema = z.object({
  url: z.string().optional(),
  db: z.number().default(0),
});

const GatewayConfigSchema = z.object({
  rate_limit_per_tenant_requests:   z.number().int().positive().default(1000),
  rate_limit_per_tenant_window_ms:  z.number().int().positive().default(60_000),
  rate_limit_per_provider_requests: z.number().int().positive().default(500),
  rate_limit_per_provider_window_ms:z.number().int().positive().default(60_000),
  health_check_interval_ms:         z.number().int().positive().default(60_000),
  default_max_tokens:               z.number().int().positive().default(4096),
});

export const ConfigSchema = z.object({
  profile: ProfileSchema,
  providers: z.object({
    anthropic: ProviderConfigSchema.optional(),
    openai: ProviderConfigSchema.optional(),
    mistral: ProviderConfigSchema.optional(),
  }),
  vllm: VLLMConfigSchema.optional(),
  redaction: RedactionConfigSchema,
  telemetry: TelemetryConfigSchema,
  database: DatabaseConfigSchema,
  redis: RedisConfigSchema,
  gateway: GatewayConfigSchema,
  features: z.object({
    allow_cloud_tier2: z.boolean().default(true),
    allow_cloud_tier3: z.boolean().default(false),
    dlp_enabled: z.boolean().default(true),
  }),
});

export type Config = z.infer<typeof ConfigSchema>;

export function loadConfig(): Config {
  const profileFile = process.env.NEXUS_LLM_POLICY_FILE
    ? readFileSync(resolve(process.env.NEXUS_LLM_POLICY_FILE), "utf-8")
    : "{}";

  const envOverrides = {
    profile: process.env.NEXUS_PROFILE,
    providers: {
      anthropic: process.env.ANTHROPIC_API_KEY
        ? {
            enabled: process.env.ANTHROPIC_PROVIDER_ENABLED !== "false",
            api_key: process.env.ANTHROPIC_API_KEY,
            base_url: process.env.ANTHROPIC_BASE_URL,
          }
        : undefined,
      openai: process.env.OPENAI_API_KEY
        ? {
            enabled: process.env.OPENAI_PROVIDER_ENABLED !== "false",
            api_key: process.env.OPENAI_API_KEY,
            base_url: process.env.OPENAI_BASE_URL,
          }
        : undefined,
      mistral: process.env.MISTRAL_API_KEY
        ? {
            enabled: process.env.MISTRAL_PROVIDER_ENABLED !== "false",
            api_key: process.env.MISTRAL_API_KEY,
          }
        : undefined,
    },
    vllm: process.env.VLLM_BASE_URL
      ? {
          base_url: process.env.VLLM_BASE_URL,
          api_key: process.env.VLLM_API_KEY,
          model_name: process.env.VLLM_MODEL_NAME,
        }
      : undefined,
    database: {
      url: process.env.POSTGRES_URL ?? process.env.DATABASE_URL,
    },
    redis: {
      url: process.env.REDIS_URL,
    },
    gateway: {
      rate_limit_per_tenant_requests:    process.env.RATE_LIMIT_PER_TENANT_REQUESTS    ? Number(process.env.RATE_LIMIT_PER_TENANT_REQUESTS)    : undefined,
      rate_limit_per_tenant_window_ms:   process.env.RATE_LIMIT_PER_TENANT_WINDOW_MS   ? Number(process.env.RATE_LIMIT_PER_TENANT_WINDOW_MS)   : undefined,
      rate_limit_per_provider_requests:  process.env.RATE_LIMIT_PER_PROVIDER_REQUESTS  ? Number(process.env.RATE_LIMIT_PER_PROVIDER_REQUESTS)  : undefined,
      rate_limit_per_provider_window_ms: process.env.RATE_LIMIT_PER_PROVIDER_WINDOW_MS ? Number(process.env.RATE_LIMIT_PER_PROVIDER_WINDOW_MS) : undefined,
      health_check_interval_ms:          process.env.HEALTH_CHECK_INTERVAL_MS          ? Number(process.env.HEALTH_CHECK_INTERVAL_MS)          : undefined,
      default_max_tokens:                process.env.DEFAULT_MAX_TOKENS                ? Number(process.env.DEFAULT_MAX_TOKENS)                : undefined,
    },
  };

  const merged = {
    ...parseYaml(profileFile),
    ...envOverrides,
  };

  return ConfigSchema.parse(merged);
}
