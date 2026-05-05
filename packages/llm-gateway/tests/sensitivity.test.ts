import { describe, it, expect } from "vitest";
import { SecretScanner } from "../src/router/secret-scanner.js";
import { SensitivityClassifier } from "../src/router/sensitivity-classifier.js";
import type { LLMMessage } from "../src/types.js";

// ─── SecretScanner ───────────────────────────────────────────────────────────

describe("SecretScanner", () => {
  const scanner = new SecretScanner();

  it("rileva AWS access key", () => {
    const r = scanner.scan("La chiave è AKIAIOSFODNN7EXAMPLE e va tenuta segreta");
    expect(r.found).toBe(true);
    expect(r.patterns.some((p) => p.type === "aws_key")).toBe(true);
    expect(r.max_tier).toBe(3);
  });

  it("rileva GitHub PAT", () => {
    const r = scanner.scan("Usa ghp_abcdefghijklmnopqrstuvwxyz123456 per autenticarti");
    expect(r.found).toBe(true);
    expect(r.patterns.some((p) => p.type === "github_pat")).toBe(true);
    expect(r.max_tier).toBe(3);
  });

  it("rileva PEM private key", () => {
    const r = scanner.scan("-----BEGIN RSA PRIVATE KEY----- abc");
    expect(r.found).toBe(true);
    expect(r.patterns.some((p) => p.type === "pem_private_key")).toBe(true);
    expect(r.max_tier).toBe(3);
  });

  it("rileva connection string DB", () => {
    const r = scanner.scan("Connessione: postgres://admin:secret@db.example.com:5432/prod");
    expect(r.found).toBe(true);
    expect(r.patterns.some((p) => p.type === "db_connection_string")).toBe(true);
    expect(r.max_tier).toBe(3);
  });

  it("rileva JWT", () => {
    const jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
    const r = scanner.scan(`Token: ${jwt}`);
    expect(r.found).toBe(true);
    expect(r.patterns.some((p) => p.type === "jwt")).toBe(true);
  });

  it("rileva codice fiscale italiano", () => {
    const r = scanner.scan("Il CF del paziente è RSSMRA85M01H501Z");
    expect(r.found).toBe(true);
    expect(r.patterns.some((p) => p.type === "italian_cf")).toBe(true);
    expect(r.max_tier).toBe(3);
  });

  it("rileva email come tier 2", () => {
    const r = scanner.scan("Contatta mario.rossi@example.com per info");
    expect(r.found).toBe(true);
    expect(r.patterns.some((p) => p.type === "email_pii")).toBe(true);
    expect(r.max_tier).toBe(2);
  });

  it("testo pulito → tier 0", () => {
    const r = scanner.scan("Scrivi una funzione TypeScript per ordinare un array");
    expect(r.found).toBe(false);
    expect(r.max_tier).toBe(0);
  });

  it("redact sostituisce il pattern trovato", () => {
    const { redacted, count } = scanner.redact(
      "Token ghp_abcdefghijklmnopqrstuvwxyz123456 è scaduto"
    );
    expect(redacted).toContain("[REDACTED:github_pat]");
    expect(count).toBeGreaterThan(0);
  });
});

// ─── SensitivityClassifier ───────────────────────────────────────────────────

describe("SensitivityClassifier — classifySync", () => {
  // Usa classifySync per evitare dipendenza da Presidio nei test
  const classifier = new SensitivityClassifier("localhost:50051");

  const msg = (content: string): LLMMessage[] => [{ role: "user", content }];

  it("prompt senza dati sensibili → tier 0", () => {
    const r = classifier.classifySync(msg("Come faccio il deploy di un container Docker?"));
    expect(r.tier).toBe(0);
    expect(r.reasons).toHaveLength(0);
  });

  it("prompt con email → tier 2", () => {
    const r = classifier.classifySync(msg("Manda un'email a test@example.com con il contratto"));
    expect(r.tier).toBe(2);
  });

  it("prompt con AWS key → tier 3", () => {
    const r = classifier.classifySync(msg("Chiave: AKIAIOSFODNN7EXAMPLE — usala per S3"));
    expect(r.tier).toBe(3);
    expect(r.secret_patterns).toContain("aws_key");
  });

  it("prompt con CF → tier 3 (mai al cloud)", () => {
    const r = classifier.classifySync(msg("Il paziente RSSMRA85M01H501Z ha avuto ricovero"));
    expect(r.tier).toBe(3);
    expect(r.secret_patterns).toContain("italian_cf");
  });

  it("multi-message: un messaggio pulito + uno con secret → tier 3", () => {
    const messages: LLMMessage[] = [
      { role: "system", content: "Sei un assistente." },
      { role: "user", content: "Ecco la chiave: AKIAIOSFODNN7EXAMPLE" },
    ];
    const r = classifier.classifySync(messages);
    expect(r.tier).toBe(3);
  });
});

// ─── Acceptance: tier 3 non raggiunge provider cloud ────────────────────────

describe("Acceptance: prompt con PII bloccato prima del provider", () => {
  it("classifySync restituisce tier 3 su CF", () => {
    const classifier = new SensitivityClassifier("localhost:50051");
    const r = classifier.classifySync([
      { role: "user", content: "Analizza il paziente RSSMRA85M01H501Z, nato il 01/05/1985" },
    ]);
    // tier 3 → la policy cloud deve bloccare
    expect(r.tier).toBe(3);
    expect(r.reasons.length).toBeGreaterThan(0);
  });
});
