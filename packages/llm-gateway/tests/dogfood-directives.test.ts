/**
 * dogfood-directives.test.ts — Conformità LLM gateway al piano dogfooding (Fase 4).
 *
 * Verifica che il gateway Nexus rispetti le stesse direttive che impone ai
 * progetti generati (ref: migration 0037 direttive G — routing sensibilità).
 *
 * Nessun mock HTTP esterno: i test usano le classi core direttamente
 * (PolicyEngine, SecretScanner, SensitivityClassifier.classifySync) isolando
 * il comportamento di routing e redaction dal network layer.
 */

import { describe, it, expect } from "vitest";
import { PolicyEngine } from "../src/router/policy-engine.js";
import { SecretScanner } from "../src/router/secret-scanner.js";
import { SensitivityClassifier } from "../src/router/sensitivity-classifier.js";
import type { LLMMessage } from "../src/types.js";

// ─── 4.1 Tier 3 non raggiunge provider cloud ─────────────────────────────────

describe("4.1 — tier 3: mai in cloud (direttiva #28)", () => {
  const engine = new PolicyEngine("config/policies/default.yaml");

  it("profilo cloud: tier 3 risulta blocked", () => {
    const decision = engine.decide(3, "");
    expect(decision.blocked).toBe(true);
    expect(decision.providers).toHaveLength(0);
    expect(decision.reason).toMatch(/tier 3/i);
  });

  it("profilo cloud: tier 0 e tier 1 non sono blocked", () => {
    expect(engine.decide(0, "").blocked).toBe(false);
    expect(engine.decide(1, "").blocked).toBe(false);
  });

  it("profilo onprem: tier 3 va a vllm, non a provider cloud", () => {
    const onprem = new PolicyEngine("config/policies/onprem.yaml");
    const decision = onprem.decide(3, "");
    expect(decision.blocked).toBe(false);
    const cloudProviders = ["anthropic", "openai", "mistral", "deepseek", "google"];
    for (const provider of decision.providers) {
      expect(cloudProviders).not.toContain(provider);
    }
    expect(decision.providers).toContain("vllm");
  });
});

// ─── 4.2 Audit trail: PII non trapela (direttiva #31) ────────────────────────

describe("4.2 — marker PII redatto prima del dispatch (direttiva #31)", () => {
  const scanner = new SecretScanner();

  it("bearer token sk-* viene redatto (generic_api_key con formato key=value)", () => {
    // Il pattern generic_api_key richiede prefisso + separatore (= o :) + valore
    const input = "Imposta: bearer=sk-LEAKSECRET123abcdef1234567890";
    const { redacted, count } = scanner.redact(input);
    expect(count).toBeGreaterThan(0);
    expect(redacted).not.toContain("sk-LEAKSECRET123abcdef1234567890");
    expect(redacted).toContain("[REDACTED:");
  });

  it("testo senza segreti: nessuna redaction, count = 0", () => {
    const input = "Scrivi una funzione TypeScript per ordinare un array di numeri";
    const { redacted, count } = scanner.redact(input);
    expect(count).toBe(0);
    expect(redacted).toBe(input);
  });

  it("GitHub PAT redatto e non presente nell'output", () => {
    const pat = "ghp_abcdefghijklmnopqrstuvwxyz123456";
    const { redacted } = scanner.redact(`Token: ${pat}`);
    expect(redacted).not.toContain(pat);
    expect(redacted).toContain("[REDACTED:github_pat]");
  });
});

// ─── 4.3 Feature flag allow_cloud_tier2/tier3 (direttiva #32) ────────────────

describe("4.3 — feature flag: tier 2 bloccato by default nel profilo cloud (direttiva #32)", () => {
  it("profilo cloud default: tier 2 bloccato (blocked: true nel routing)", () => {
    const engine = new PolicyEngine("config/policies/default.yaml");
    expect(engine.decide(2, "").blocked).toBe(true);
  });

  it("profilo onprem: tier 2 instradato su vllm, non bloccato", () => {
    const engine = new PolicyEngine("config/policies/onprem.yaml");
    const decision = engine.decide(2, "");
    expect(decision.blocked).toBe(false);
    expect(decision.providers).toContain("vllm");
  });

  it("profilo onprem: nessun provider cloud raggiungibile per nessun tier", () => {
    const engine = new PolicyEngine("config/policies/onprem.yaml");
    const cloudProviders = ["anthropic", "openai", "mistral", "deepseek", "google"];
    for (const tier of [0, 1, 2, 3] as const) {
      const { providers } = engine.decide(tier, "");
      for (const provider of providers) {
        expect(cloudProviders).not.toContain(provider);
      }
    }
  });
});

// ─── 4.4 Redaction input sensibili (direttiva #30) ───────────────────────────

describe("4.4 — redaction PII obbligatoria (direttiva #30)", () => {
  const scanner = new SecretScanner();

  it("email rilevata come tier 2 e redatta", () => {
    const input = "Contatta email@test.com per info";
    const scanResult = scanner.scan(input);
    expect(scanResult.found).toBe(true);
    expect(scanResult.patterns.some((p) => p.type === "email_pii")).toBe(true);
    expect(scanResult.max_tier).toBe(2);
    const { redacted } = scanner.redact(input);
    expect(redacted).not.toContain("email@test.com");
  });

  it("IBAN italiano rilevato come tier 3 e redatto", () => {
    const iban = "IT60X0542811101000000123456";
    const input = `Bonifico verso ${iban} entro domani`;
    const scanResult = scanner.scan(input);
    expect(scanResult.found).toBe(true);
    expect(scanResult.patterns.some((p) => p.type === "italian_iban")).toBe(true);
    expect(scanResult.max_tier).toBe(3);
    const { redacted } = scanner.redact(input);
    expect(redacted).not.toContain(iban);
  });

  it("chiave API (access_token) rilevata e redatta", () => {
    // Usa access_token= — formato supportato dal pattern generic_api_key
    const key = "sk-LEAKSECRET123xyz456abcdef78901";
    const input = `access_token=${key}`;
    const { redacted, count } = scanner.redact(input);
    expect(count).toBeGreaterThan(0);
    expect(redacted).not.toContain(key);
  });

  it("input con email + IBAN: entrambi redatti, count >= 2", () => {
    const input =
      "Paga IT60X0542811101000000123456 e notifica mario@example.com";
    const { redacted, count } = scanner.redact(input);
    expect(count).toBeGreaterThanOrEqual(2);
    expect(redacted).not.toContain("IT60X0542811101000000123456");
    expect(redacted).not.toContain("mario@example.com");
  });
});

// ─── 4.5 Classificazione sincrona coerente (direttiva #28) ───────────────────

describe("4.5 — classifySync coerenza tier (direttiva #28)", () => {
  const classifier = new SensitivityClassifier("localhost:50051");

  it("messaggio pulito → tier 0", () => {
    const messages: LLMMessage[] = [
      { role: "user", content: "Scrivi una funzione che somma due numeri" },
    ];
    const result = classifier.classifySync(messages);
    expect(result.tier).toBe(0);
    expect(result.reasons).toHaveLength(0);
  });

  it("messaggio con GitHub PAT → tier 3", () => {
    const messages: LLMMessage[] = [
      { role: "user", content: "Token: ghp_abcdefghijklmnopqrstuvwxyz123456" },
    ];
    const result = classifier.classifySync(messages);
    expect(result.tier).toBe(3);
    expect(result.secret_patterns.some((p) => p.includes("github_pat"))).toBe(true);
  });

  it("messaggio con email → tier >= 2", () => {
    const messages: LLMMessage[] = [
      { role: "user", content: "Contatta utente@nexus.ai per assistenza" },
    ];
    const result = classifier.classifySync(messages);
    expect(result.tier).toBeGreaterThanOrEqual(2);
  });
});
