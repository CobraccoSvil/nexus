/**
 * Test golden corpus per la redaction pipeline.
 * Acceptance: nessun pattern sensibile noto attraversa la redaction.
 *
 * Ogni caso definisce input, il tipo di pattern atteso e verifica che
 * l'output non contenga più il valore originale.
 */
import { describe, it, expect } from "vitest";
import { SecretScanner } from "../src/router/secret-scanner.js";
import { CodeAnonymizer } from "../src/redaction/code-anonymizer.js";
import { PathPolicy } from "../src/redaction/path-policy.js";
import { RedactionMap } from "../src/redaction/redaction-map.js";

// ─── SecretScanner redact corpus ─────────────────────────────────────────────

interface GoldenCase {
  label: string;
  input: string;
  mustNotContain: string[];
  mustContainType?: string;
}

const GOLDEN_CORPUS: GoldenCase[] = [
  {
    label: "AWS access key in commento",
    input: "// AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE",
    mustNotContain: ["AKIAIOSFODNN7EXAMPLE"],
    mustContainType: "aws_key",
  },
  {
    label: "GitHub PAT in env export",
    input: "export GH_TOKEN=ghp_abcdefghijklmnopqrstu1234567890",
    mustNotContain: ["ghp_abcdefghijklmnopqrstu1234567890"],
    mustContainType: "github_pat",
  },
  {
    label: "GitLab token in .env",
    input: "GITLAB_TOKEN=glpat-xxxxxxxxxxxxxxxxxxxx",
    mustNotContain: ["glpat-xxxxxxxxxxxxxxxxxxxx"],
    mustContainType: "gitlab_token",
  },
  {
    label: "PEM private key header",
    input: "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA...",
    mustNotContain: ["BEGIN RSA PRIVATE KEY"],
    mustContainType: "pem_private_key",
  },
  {
    label: "PEM EC private key",
    input: "-----BEGIN EC PRIVATE KEY-----\nMHQCAQEEIBkg...",
    mustNotContain: ["BEGIN EC PRIVATE KEY"],
    mustContainType: "pem_private_key",
  },
  {
    label: "Postgres connection string",
    input: "DATABASE_URL=postgres://admin:sup3rs3cr3t@prod.db.internal:5432/myapp",
    mustNotContain: ["sup3rs3cr3t"],
    mustContainType: "db_connection_string",
  },
  {
    label: "MongoDB connection string",
    input: "MONGO_URI=mongodb://user:pass123@cluster.mongo.com/db",
    mustNotContain: ["pass123"],
    mustContainType: "db_connection_string",
  },
  {
    label: "Redis connection string",
    input: "redis://default:myredispassword@redis.internal:6379",
    mustNotContain: ["myredispassword"],
    mustContainType: "db_connection_string",
  },
  {
    label: "JWT token nel corpo",
    input: "Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiJ1c2VyMTIzIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c",
    mustNotContain: ["eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9"],
    mustContainType: "jwt",
  },
  {
    label: "Codice fiscale italiano",
    input: "Paziente: RSSMRA85M01H501Z, ricovero urgente",
    mustNotContain: ["RSSMRA85M01H501Z"],
    mustContainType: "italian_cf",
  },
  {
    label: "IBAN italiano",
    input: "Bonifico su IT60X0542811101000000123456",
    mustNotContain: ["IT60X0542811101000000123456"],
    mustContainType: "italian_iban",
  },
  {
    label: "Carta di credito Visa",
    input: "Pagamento con 4532015112830366",
    mustNotContain: ["4532015112830366"],
    mustContainType: "credit_card",
  },
  {
    label: "Email PII",
    input: "Contatta mario.bianchi@azienda-cliente.it per il contratto",
    mustNotContain: ["mario.bianchi@azienda-cliente.it"],
    mustContainType: "email_pii",
  },
  {
    label: "API key generica in YAML",
    input: "api_key: 'sk-proj-AbCdEfGhIjKlMnOpQrStUvWxYz123456'",
    mustNotContain: ["sk-proj-AbCdEfGhIjKlMnOpQrStUvWxYz123456"],
    mustContainType: "generic_api_key",
  },
  {
    label: "Segreto in commento multi-riga",
    input: `/*
 * Chiave produzione: AKIAIOSFODNN7EXAMPLE
 * Non committare!
 */`,
    mustNotContain: ["AKIAIOSFODNN7EXAMPLE"],
    mustContainType: "aws_key",
  },
  {
    label: "GCP service account JSON",
    input: `{
  "type": "service_account",
  "project_id": "my-project"
}`,
    // Il pattern redige il match completo "type": "service_account" — il placeholder contiene
    // la parola come tipo, ma la stringa JSON originale non è più presente
    mustNotContain: ['"type": "service_account"'],
    mustContainType: "gcp_service_account",
  },
  {
    label: "Testo pulito — nessuna redaction",
    input: "Come implemento un binary search in TypeScript?",
    mustNotContain: [],
  },
  {
    label: "Codice TypeScript senza segreti",
    input: `function greet(name: string): string {
  return \`Hello, \${name}!\`;
}`,
    mustNotContain: [],
  },
];

describe("SecretScanner — golden corpus", () => {
  const scanner = new SecretScanner();

  for (const tc of GOLDEN_CORPUS) {
    it(tc.label, () => {
      const { redacted, count } = scanner.redact(tc.input);

      if (tc.mustNotContain.length === 0) {
        // Testo pulito: nessuna modifica attesa
        expect(count).toBe(0);
        return;
      }

      // Ogni valore sensibile deve essere assente nell'output
      for (const forbidden of tc.mustNotContain) {
        expect(redacted).not.toContain(forbidden);
      }

      // Se specificato, verifica che il tipo corretto sia stato trovato
      if (tc.mustContainType) {
        const result = scanner.scan(tc.input);
        expect(result.patterns.some((p) => p.type === tc.mustContainType)).toBe(true);
      }
    });
  }
});

// ─── RedactionMap ─────────────────────────────────────────────────────────────

describe("RedactionMap", () => {
  it("store + rehydrate round-trip", () => {
    const map = new RedactionMap("req-1");
    const placeholder = map.store("sup3rs3cr3t", "db_password");
    expect(placeholder).toMatch(/^__NEXUS_DB_PASSWORD_\d+__$/);

    const rehydrated = map.rehydrate(`password: ${placeholder}`);
    expect(rehydrated).toContain("sup3rs3cr3t");
    expect(rehydrated).not.toContain(placeholder);
  });

  it("dedup: stesso valore → stesso placeholder", () => {
    const map = new RedactionMap("req-2");
    const p1 = map.store("secret", "token");
    const p2 = map.store("secret", "token");
    expect(p1).toBe(p2);
    expect(map.size()).toBe(1);
  });

  it("valori diversi → placeholder diversi", () => {
    const map = new RedactionMap("req-3");
    const p1 = map.store("secret-a", "token");
    const p2 = map.store("secret-b", "token");
    expect(p1).not.toBe(p2);
    expect(map.size()).toBe(2);
  });

  it("auditSnapshot non espone valori originali", () => {
    const map = new RedactionMap("req-4");
    map.store("top-secret-value", "api_key");
    const snap = map.auditSnapshot();
    expect(snap.length).toBe(1);
    expect(JSON.stringify(snap)).not.toContain("top-secret-value");
  });
});

// ─── PathPolicy ──────────────────────────────────────────────────────────────

describe("PathPolicy", () => {
  const policy = new PathPolicy();

  it(".env è bloccato", () => {
    expect(policy.checkPath(".env")).toBe("blocked");
    expect(policy.checkPath("apps/api/.env.production")).toBe("blocked");
  });

  it("file PEM è bloccato", () => {
    expect(policy.checkPath("infra/certs/server.pem")).toBe("blocked");
    expect(policy.checkPath("id_rsa")).toBe("blocked");
  });

  it("service account JSON è bloccato", () => {
    expect(policy.checkPath("service-account-prod.json")).toBe("blocked");
  });

  it("README.md è in whitelist", () => {
    expect(policy.checkPath("README.md")).toBe("whitelisted");
  });

  it("docs markdown è in whitelist", () => {
    expect(policy.checkPath("docs/architecture.md")).toBe("whitelisted");
  });

  it("file sorgente normale → redact", () => {
    expect(policy.checkPath("src/components/Button.tsx")).toBe("redact");
    expect(policy.checkPath("packages/api/src/handler.ts")).toBe("redact");
  });
});

// ─── CodeAnonymizer ──────────────────────────────────────────────────────────

describe("CodeAnonymizer", () => {
  const anonymizer = new CodeAnonymizer();

  it("anonimizza assignment con parola chiave secret", () => {
    const map = new RedactionMap("req-5");
    const input = `const password = "MyS3cr3tP@ssw0rd123!"`;
    const { text, count } = anonymizer.anonymize(input, map);
    expect(count).toBeGreaterThan(0);
    expect(text).not.toContain("MyS3cr3tP@ssw0rd123!");
  });

  it("reidrata correttamente dopo anonymize", () => {
    const map = new RedactionMap("req-6");
    const input = `const api_key = "super-secret-api-key-value-here"`;
    const { text } = anonymizer.anonymize(input, map);
    const rehydrated = map.rehydrate(text);
    expect(rehydrated).toContain("super-secret-api-key-value-here");
  });

  it("testo senza segreti → count 0", () => {
    const map = new RedactionMap("req-7");
    const { count } = anonymizer.anonymize("const x = 1 + 2;", map);
    expect(count).toBe(0);
  });
});
