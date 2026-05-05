import { describe, it, expect, vi, beforeEach } from "vitest";
import { LLMGateway } from "../src/gateway.js";
import type { LLMRequest } from "../src/types.js";

// Mock providers
vi.mock("../src/providers/anthropic.js", () => ({
  AnthropicProvider: vi.fn().mockImplementation(() => ({
    name: "anthropic",
    supports_tools: true,
    supports_streaming: true,
    max_context_tokens: 200_000,
    tier_compatibility: [0, 1, 2],
    complete: vi.fn().mockResolvedValue({
      content: "Risposta Anthropic",
      usage: { input_tokens: 10, output_tokens: 5 },
      model_used: "claude-sonnet-4",
      provider_used: "anthropic",
      latency_ms: 100,
      finish_reason: "stop",
    }),
    stream: vi.fn(),
    healthcheck: vi.fn().mockResolvedValue(true),
  })),
}));

vi.mock("../src/providers/openai.js", () => ({
  OpenAIProvider: vi.fn().mockImplementation(() => ({
    name: "openai",
    supports_tools: true,
    supports_streaming: true,
    max_context_tokens: 128_000,
    tier_compatibility: [0, 1, 2],
    complete: vi.fn().mockResolvedValue({
      content: "Risposta OpenAI",
      usage: { input_tokens: 10, output_tokens: 5 },
      model_used: "gpt-4o",
      provider_used: "openai",
      latency_ms: 80,
      finish_reason: "stop",
    }),
    stream: vi.fn(),
    healthcheck: vi.fn().mockResolvedValue(true),
  })),
}));

vi.mock("../src/providers/mistral.js", () => ({
  MistralProvider: vi.fn().mockImplementation(() => ({
    name: "mistral",
    supports_tools: true,
    supports_streaming: true,
    max_context_tokens: 128_000,
    tier_compatibility: [0, 1, 2],
    complete: vi.fn().mockResolvedValue({
      content: "Risposta Mistral",
      usage: { input_tokens: 10, output_tokens: 5 },
      model_used: "mistral-small-latest",
      provider_used: "mistral",
      latency_ms: 90,
      finish_reason: "stop",
    }),
    stream: vi.fn(),
    healthcheck: vi.fn().mockResolvedValue(true),
  })),
}));

vi.mock("../src/providers/vllm-local.js", () => ({
  VLLMProvider: vi.fn().mockImplementation(() => ({
    name: "vllm",
    supports_tools: true,
    supports_streaming: true,
    max_context_tokens: 32_768,
    tier_compatibility: [0, 1, 2, 3],
    complete: vi.fn().mockResolvedValue({
      content: "Risposta vLLM",
      usage: { input_tokens: 10, output_tokens: 5 },
      model_used: "qwen-32b",
      provider_used: "vllm",
      latency_ms: 200,
      finish_reason: "stop",
    }),
    stream: vi.fn(),
    healthcheck: vi.fn().mockResolvedValue(true),
  })),
}));

vi.mock("../src/router/model-alias-resolver.js", () => ({
  ModelAliasResolver: vi.fn().mockImplementation(() => ({
    resolve: vi.fn().mockReturnValue("claude-sonnet-4"),
    getEntry: vi.fn(),
    listAliases: vi.fn().mockReturnValue([]),
  })),
}));

vi.mock("../src/router/sensitivity-classifier.js", () => ({
  SensitivityClassifier: vi.fn().mockImplementation(() => ({
    classify: vi.fn().mockResolvedValue({ tier: 0, reasons: [], secret_patterns: [], presidio_entities: [] }),
    classifySync: vi.fn().mockReturnValue({ tier: 0, reasons: [], secret_patterns: [], presidio_entities: [] }),
  })),
}));

vi.mock("../src/router/policy-engine.js", () => ({
  PolicyEngine: vi.fn().mockImplementation((file: string) => ({
    decide: vi.fn().mockImplementation((tier: number) => {
      if (tier === 3 && file.includes("default")) {
        return { providers: [], blocked: true, reason: "Tier 3 bloccato" };
      }
      return { providers: ["anthropic", "openai"], blocked: false };
    }),
    validateTierClaim: vi.fn(),
    getProfile: vi.fn().mockReturnValue("cloud"),
  })),
}));

vi.mock("../src/router/rate-limiter.js", () => ({
  RateLimiter: vi.fn().mockImplementation(() => ({
    checkTenant: vi.fn(),
    checkProvider: vi.fn(),
  })),
}));

const POLICY_FILE: Record<string, string> = {
  cloud: "./config/policies/default.yaml",
  hybrid: "./config/policies/hybrid.yaml",
  onprem: "./config/policies/onprem.yaml",
};

const makeConfig = (profile: "cloud" | "hybrid" | "onprem" = "cloud") => ({
  config: {
    profile,
    providers: {
      anthropic: { enabled: true, api_key: "test-ant-key", timeout_ms: 30000 },
      openai: { enabled: true, api_key: "test-oai-key", timeout_ms: 30000 },
      mistral: { enabled: true, api_key: "test-mis-key", timeout_ms: 30000 },
    },
    vllm:
      profile !== "cloud"
        ? {
            base_url: "http://localhost:8000/v1",
            api_key: "",
            model_name: "Qwen",
            max_context_tokens: 32768,
          }
        : undefined,
    redaction: { enabled: false, strict_mode: false, presidio_grpc_url: "localhost:50051", redaction_ttl_ms: 300000 },
    telemetry: { enabled: false, otlp_endpoint: "", log_level: "info" as const, service_name: "test" },
    database: { url: "postgres://test", pool_size: 5, ssl: false },
    redis: { url: "redis://test" },
    features: { allow_cloud_tier2: true, allow_cloud_tier3: false, dlp_enabled: false },
    gateway: { rate_limit_per_tenant_requests: 1000, rate_limit_per_tenant_window_ms: 60000, rate_limit_per_provider_requests: 500, rate_limit_per_provider_window_ms: 60000 },
  } as any,
  aliasesFile: "./config/model-aliases.yaml",
  policyFile: POLICY_FILE[profile],
});

const makeRequest = (overrides: Partial<LLMRequest> = {}): LLMRequest => ({
  model: "coder-large",
  messages: [{ role: "user", content: "test" }],
  metadata: {
    tenant_id: "t1",
    user_id: "u1",
    request_id: "r1",
    sensitivity_tier: 0,
    feature: "test",
  },
  ...overrides,
});

describe("LLMGateway", () => {
  it("cloud profile registra 3 provider cloud", () => {
    const gw = new LLMGateway(makeConfig("cloud"));
    const providers = gw.getRegisteredProviders();
    expect(providers).toContain("anthropic");
    expect(providers).toContain("openai");
    expect(providers).toContain("mistral");
    expect(providers).not.toContain("vllm");
  });

  it("hybrid profile registra cloud + vllm", () => {
    const gw = new LLMGateway(makeConfig("hybrid"));
    const providers = gw.getRegisteredProviders();
    expect(providers).toContain("anthropic");
    expect(providers).toContain("vllm");
  });

  it("onprem profile registra solo vllm", () => {
    const gw = new LLMGateway(makeConfig("onprem"));
    const providers = gw.getRegisteredProviders();
    expect(providers).not.toContain("anthropic");
    expect(providers).not.toContain("openai");
    expect(providers).toContain("vllm");
  });

  it("complete ritorna risposta valida per tier 0", async () => {
    const gw = new LLMGateway(makeConfig());
    const result = await gw.complete(makeRequest());
    expect(result.content).toBeTruthy();
    expect(result.provider_used).toBeTruthy();
    expect(result.finish_reason).toBe("stop");
  });

  it("tier 3 bloccato in profilo cloud", async () => {
    const gw = new LLMGateway(makeConfig("cloud"));
    await expect(
      gw.complete(makeRequest({ metadata: { tenant_id: "t1", user_id: "u1", request_id: "r2", sensitivity_tier: 3, feature: "test" } }))
    ).rejects.toThrow();
  });

  it("getProviderStatuses ritorna status iniziale healthy", () => {
    const gw = new LLMGateway(makeConfig());
    const statuses = gw.getProviderStatuses();
    expect(statuses.length).toBeGreaterThan(0);
    statuses.forEach((s) => expect(s.healthy).toBe(true));
  });
});
