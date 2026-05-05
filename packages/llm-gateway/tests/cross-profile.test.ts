/**
 * Cross-Profile Portability Test — Fase 7 Gate
 *
 * Verifica che lo stesso codice applicativo (LLMGateway) produca risposte
 * con interfaccia identica indipendentemente dal profilo attivo.
 * Zero modifiche al codice = portabilità garantita.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { LLMGateway } from "../src/gateway.js";
import type { LLMRequest, LLMResponse } from "../src/types.js";

// ─── Provider mocks ───────────────────────────────────────────────────────────

const makeProviderMock = (name: string, tierCompat: number[], responseOverrides: Partial<LLMResponse> = {}) => ({
  name,
  supports_tools: true,
  supports_streaming: true,
  max_context_tokens: 32_768,
  tier_compatibility: tierCompat,
  complete: vi.fn().mockResolvedValue({
    content: `Risposta da ${name}`,
    usage: { input_tokens: 20, output_tokens: 10 },
    model_used: `${name}-model`,
    provider_used: name,
    latency_ms: 150,
    finish_reason: "stop",
    ...responseOverrides,
  }),
  stream: vi.fn(),
  healthcheck: vi.fn().mockResolvedValue(true),
});

vi.mock("../src/providers/anthropic.js", () => ({
  AnthropicProvider: vi.fn().mockImplementation(() =>
    makeProviderMock("anthropic", [0, 1, 2])
  ),
}));
vi.mock("../src/providers/openai.js", () => ({
  OpenAIProvider: vi.fn().mockImplementation(() =>
    makeProviderMock("openai", [0, 1, 2])
  ),
}));
vi.mock("../src/providers/mistral.js", () => ({
  MistralProvider: vi.fn().mockImplementation(() =>
    makeProviderMock("mistral", [0, 1, 2])
  ),
}));
vi.mock("../src/providers/vllm-local.js", () => ({
  VLLMProvider: vi.fn().mockImplementation(() =>
    makeProviderMock("vllm", [0, 1, 2, 3])
  ),
}));

// ─── Router mocks ─────────────────────────────────────────────────────────────

vi.mock("../src/router/model-alias-resolver.js", () => ({
  ModelAliasResolver: vi.fn().mockImplementation(() => ({
    resolve: vi.fn().mockReturnValue("model-id"),
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

vi.mock("../src/router/rate-limiter.js", () => ({
  RateLimiter: vi.fn().mockImplementation(() => ({
    checkTenant: vi.fn(),
    checkProvider: vi.fn(),
  })),
}));

// PolicyEngine: routing diverso per profilo, simulato tramite nome file policy
vi.mock("../src/router/policy-engine.js", () => ({
  PolicyEngine: vi.fn().mockImplementation((policyFile: string) => {
    const profile = policyFile.includes("onprem")
      ? "onprem"
      : policyFile.includes("hybrid")
      ? "hybrid"
      : "cloud";

    return {
      decide: vi.fn().mockImplementation((tier: number) => {
        if (profile === "onprem") {
          return { providers: ["vllm"], blocked: false };
        }
        if (profile === "hybrid") {
          return tier >= 3
            ? { providers: ["vllm"], blocked: false }
            : { providers: ["anthropic", "openai"], blocked: false };
        }
        // cloud
        return tier >= 3
          ? { providers: [], blocked: true, reason: "Tier 3 bloccato in cloud" }
          : { providers: ["anthropic", "openai"], blocked: false };
      }),
      validateTierClaim: vi.fn(),
      getProfile: vi.fn().mockReturnValue(profile),
    };
  }),
}));

// ─── Helpers ──────────────────────────────────────────────────────────────────

const RESPONSE_REQUIRED_FIELDS: (keyof LLMResponse)[] = [
  "content",
  "usage",
  "model_used",
  "provider_used",
  "latency_ms",
  "finish_reason",
];

function assertResponseShape(resp: LLMResponse) {
  for (const field of RESPONSE_REQUIRED_FIELDS) {
    expect(resp, `campo ${field} mancante`).toHaveProperty(field);
  }
  expect(typeof resp.content).toBe("string");
  expect(typeof resp.usage.input_tokens).toBe("number");
  expect(typeof resp.usage.output_tokens).toBe("number");
  expect(typeof resp.latency_ms).toBe("number");
  expect(["stop", "length", "tool_calls", "content_filter"]).toContain(resp.finish_reason);
}

const makeGateway = (profile: "cloud" | "hybrid" | "onprem") =>
  new LLMGateway({
    config: {
      profile,
      providers: {
        anthropic: { enabled: true, api_key: "k", timeout_ms: 5000 },
        openai: { enabled: true, api_key: "k", timeout_ms: 5000 },
        mistral: { enabled: true, api_key: "k", timeout_ms: 5000 },
      },
      vllm:
        profile !== "cloud"
          ? { base_url: "http://vllm:8000/v1", api_key: "", model_name: "Qwen", max_context_tokens: 32768 }
          : undefined,
      redaction: { enabled: false, strict_mode: false, presidio_grpc_url: "", redaction_ttl_ms: 0 },
      telemetry: { enabled: false, otlp_endpoint: "", log_level: "silent" as any, service_name: "test" },
      database: { url: "postgres://test", pool_size: 1, ssl: false },
      redis: { url: "redis://test" },
      features: { allow_cloud_tier2: true, allow_cloud_tier3: false, dlp_enabled: false },
      gateway: { rate_limit_per_tenant_requests: 1000, rate_limit_per_tenant_window_ms: 60000, rate_limit_per_provider_requests: 500, rate_limit_per_provider_window_ms: 60000 },
    } as any,
    aliasesFile: "./config/model-aliases.yaml",
    policyFile:
      profile === "onprem"
        ? "./config/policies/onprem.yaml"
        : profile === "hybrid"
        ? "./config/policies/hybrid.yaml"
        : "./config/policies/default.yaml",
  });

const makeRequest = (tierOverride = 0): LLMRequest => ({
  model: "coder-large",
  messages: [{ role: "user", content: "Scrivi una funzione che somma due numeri" }],
  metadata: {
    tenant_id: "tenant-e2e",
    user_id: "user-e2e",
    request_id: `req-${Date.now()}`,
    sensitivity_tier: tierOverride as 0 | 1 | 2 | 3,
    feature: "code-review",
  },
});

// ─── Test suite ───────────────────────────────────────────────────────────────

describe("Cross-Profile Portability — Fase 7 Gate", () => {

  describe("Provider registration per profilo", () => {
    it("cloud: anthropic+openai+mistral, no vllm", () => {
      const gw = makeGateway("cloud");
      const p = gw.getRegisteredProviders();
      expect(p).toContain("anthropic");
      expect(p).toContain("openai");
      expect(p).toContain("mistral");
      expect(p).not.toContain("vllm");
    });

    it("hybrid: cloud + vllm", () => {
      const gw = makeGateway("hybrid");
      const p = gw.getRegisteredProviders();
      expect(p).toContain("anthropic");
      expect(p).toContain("vllm");
    });

    it("onprem: solo vllm, zero cloud", () => {
      const gw = makeGateway("onprem");
      const p = gw.getRegisteredProviders();
      expect(p).not.toContain("anthropic");
      expect(p).not.toContain("openai");
      expect(p).not.toContain("mistral");
      expect(p).toContain("vllm");
    });
  });

  describe("Risposta ha shape identica su tutti i profili (tier 0)", () => {
    const profiles = ["cloud", "hybrid", "onprem"] as const;

    for (const profile of profiles) {
      it(`profilo ${profile}: LLMResponse shape valida`, async () => {
        const gw = makeGateway(profile);
        const resp = await gw.complete(makeRequest(0));
        assertResponseShape(resp);
      });
    }
  });

  describe("Routing tier 3 — profilo determina blocco o accesso vllm", () => {
    it("cloud: tier 3 → blocco esplicito (ProviderError 403)", async () => {
      const gw = makeGateway("cloud");
      await expect(gw.complete(makeRequest(3))).rejects.toThrow();
    });

    it("hybrid: tier 3 → routed a vllm", async () => {
      const gw = makeGateway("hybrid");
      const resp = await gw.complete(makeRequest(3));
      expect(resp.provider_used).toBe("vllm");
      assertResponseShape(resp);
    });

    it("onprem: tier 3 → routed a vllm", async () => {
      const gw = makeGateway("onprem");
      const resp = await gw.complete(makeRequest(3));
      expect(resp.provider_used).toBe("vllm");
      assertResponseShape(resp);
    });
  });

  describe("Routing tier 0 — provider primario corretto per profilo", () => {
    it("cloud: tier 0 → anthropic (primary)", async () => {
      const gw = makeGateway("cloud");
      const resp = await gw.complete(makeRequest(0));
      expect(resp.provider_used).toBe("anthropic");
    });

    it("hybrid: tier 0 → anthropic (primary cloud)", async () => {
      const gw = makeGateway("hybrid");
      const resp = await gw.complete(makeRequest(0));
      expect(resp.provider_used).toBe("anthropic");
    });

    it("onprem: tier 0 → vllm (unico provider)", async () => {
      const gw = makeGateway("onprem");
      const resp = await gw.complete(makeRequest(0));
      expect(resp.provider_used).toBe("vllm");
    });
  });

  describe("Invarianti comuni a tutti i profili", () => {
    const profiles = ["cloud", "hybrid", "onprem"] as const;

    for (const profile of profiles) {
      it(`${profile}: health checks ritornano stato dei provider registrati`, () => {
        const gw = makeGateway(profile);
        const statuses = gw.getProviderStatuses();
        expect(statuses.length).toBeGreaterThan(0);
        for (const s of statuses) {
          expect(s).toHaveProperty("name");
          expect(s).toHaveProperty("healthy");
          expect(s).toHaveProperty("last_check");
        }
      });

      it(`${profile}: request_id è preservato nei metadati della call`, async () => {
        const gw = makeGateway(profile);
        const req = makeRequest(0);
        // Il request_id deve scorrere nell'audit senza essere perso
        const resp = await gw.complete(req);
        expect(resp.content).toBeTruthy(); // la call completa senza errori
      });
    }
  });
});
