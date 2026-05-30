import { describe, it, expect } from "vitest";
import { PolicyEngine } from "../src/router/policy-engine.js";

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

  it("validateTierClaim: claimed < detected → auto-elevazione (segnala, non blocca)", () => {
    // Comportamento attuale: il caller (mcp-core/brain) non puo' conoscere a
    // priori il tier di sensibilita'; e' il gateway a rilevarlo. Bloccare con
    // 400 rompeva ogni richiesta con PII. Ora si auto-eleva e si segnala soltanto
    // (il routing applica comunque le policy del tier effettivo).
    expect(() => engine.validateTierClaim(0, 3)).not.toThrow();
    expect(() => engine.validateTierClaim(1, 2)).not.toThrow();
  });
});

describe("PolicyEngine — override DLP da settings DB (regola G)", () => {
  // Mock minimale del client `postgres`: tagged template che ritorna le righe.
  const makeDb = (rows: Array<{ key: string; value: string }>) =>
    ((..._args: unknown[]) => Promise.resolve(rows)) as unknown as Parameters<
      PolicyEngine["refreshDbOverrides"]
    >[0];

  it("dlp_allow_cloud_tier3=true da DB sblocca il tier 3 (vince su tier_3.blocked YAML)", async () => {
    const engine = new PolicyEngine("./config/policies/default.yaml");
    // Default YAML: allow_cloud_tier3=false -> tier 3 bloccato
    expect(engine.decide(3, "").blocked).toBe(true);
    // DB lo abilita -> cloud permesso, decisione non bloccata
    await engine.refreshDbOverrides(makeDb([{ key: "dlp_allow_cloud_tier3", value: "true" }]), true);
    expect(engine.decide(3, "").blocked).toBe(false);
  });

  it("dlp_allow_cloud_tier2=false da DB ri-blocca il tier 2 (vince sul default YAML true)", async () => {
    const engine = new PolicyEngine("./config/policies/default.yaml");
    expect(engine.decide(2, "").blocked).toBe(false);
    await engine.refreshDbOverrides(makeDb([{ key: "dlp_allow_cloud_tier2", value: "false" }]), true);
    // Cloud escluso e nessun provider locale nel profilo cloud -> bloccato
    expect(engine.decide(2, "").blocked).toBe(true);
  });

  it("dlp_enabled=false da DB forza tier 0 (DLP disattivata)", async () => {
    const engine = new PolicyEngine("./config/policies/default.yaml");
    await engine.refreshDbOverrides(makeDb([{ key: "dlp_enabled", value: "false" }]), true);
    expect(engine.isDlpDisabled()).toBe(true);
    // Anche un tier 3 viene instradato come tier 0 (provider cloud tier 0)
    const d = engine.decide(3, "");
    expect(d.blocked).toBe(false);
    expect(d.providers[0]).toBe("openai");
  });

  it("DB down (db null) mantiene i default YAML (fallback graceful)", async () => {
    const engine = new PolicyEngine("./config/policies/default.yaml");
    await engine.refreshDbOverrides(null, true);
    expect(engine.decide(3, "").blocked).toBe(true); // resta come YAML
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
