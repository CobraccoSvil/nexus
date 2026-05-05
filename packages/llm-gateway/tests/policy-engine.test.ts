import { describe, it, expect } from "vitest";
import { PolicyEngine } from "../src/router/policy-engine.js";
import { ProviderError } from "@nexus/shared";

describe("PolicyEngine — profilo cloud (default.yaml)", () => {
  const engine = new PolicyEngine("./config/policies/default.yaml");

  it("tier 0 → openai primario", () => {
    const d = engine.decide(0, "code-review");
    expect(d.blocked).toBe(false);
    expect(d.providers[0]).toBe("openai");
  });

  it("tier 1 → lista provider non vuota", () => {
    const d = engine.decide(1, "doc-gen");
    expect(d.blocked).toBe(false);
    expect(d.providers.length).toBeGreaterThan(0);
  });

  it("tier 3 → bloccato in cloud", () => {
    const d = engine.decide(3, "code-review");
    expect(d.blocked).toBe(true);
    expect(d.reason).toContain("bloccat");
  });

  it("tenant con block_cloud=true esclude provider cloud", () => {
    const d = engine.decide(0, "code-review", { block_cloud: true });
    // Con tutti i provider cloud esclusi e nessun vllm, deve bloccare
    expect(d.blocked).toBe(true);
  });

  it("validateTierClaim: claimed >= detected → ok", () => {
    expect(() => engine.validateTierClaim(2, 1)).not.toThrow();
    expect(() => engine.validateTierClaim(3, 3)).not.toThrow();
  });

  it("validateTierClaim: claimed < detected → ProviderError", () => {
    expect(() => engine.validateTierClaim(0, 3)).toThrow(ProviderError);
    expect(() => engine.validateTierClaim(1, 2)).toThrow(ProviderError);
  });
});

describe("PolicyEngine — profilo onprem", () => {
  const engine = new PolicyEngine("./config/policies/onprem.yaml");

  it("tier 3 → vllm disponibile (non bloccato)", () => {
    const d = engine.decide(3, "code-review");
    expect(d.blocked).toBe(false);
    expect(d.providers[0]).toBe("vllm");
  });

  it("tier 0 → vllm anche per tier basso", () => {
    const d = engine.decide(0, "doc-gen");
    expect(d.providers[0]).toBe("vllm");
  });
});

describe("PolicyEngine — profilo hybrid", () => {
  const engine = new PolicyEngine("./config/policies/hybrid.yaml");

  it("tier 0 → anthropic primario", () => {
    const d = engine.decide(0, "code-review");
    expect(d.providers[0]).toBe("anthropic");
  });

  it("tier 3 → vllm primario", () => {
    const d = engine.decide(3, "code-review");
    expect(d.blocked).toBe(false);
    expect(d.providers[0]).toBe("vllm");
  });
});
