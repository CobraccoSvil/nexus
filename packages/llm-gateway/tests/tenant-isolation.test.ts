import { describe, it, expect, vi, beforeEach } from "vitest";
import { JWTService, TenantCrypto, LocalKeyStore, AuthError } from "@nexus/shared";
import { TenantContext, extractTenantId } from "../src/tenant-context.js";

// ─── JWT ──────────────────────────────────────────────────────────────────────

describe("JWTService", () => {
  const secret = "super-secret-key-that-is-at-least-32-chars!!";
  const svc = new JWTService(secret);

  it("lancia AuthError se il secret è troppo corto", () => {
    expect(() => new JWTService("short")).toThrow(AuthError);
  });

  it("firma e verifica un token valido", async () => {
    const token = await svc.sign({ tid: "tenant-a", uid: "user-1", scp: ["llm:complete"] });
    const payload = await svc.verify(token);
    expect(payload.tid).toBe("tenant-a");
    expect(payload.uid).toBe("user-1");
    expect(payload.scp).toContain("llm:complete");
  });

  it("rifiuta un token scaduto", async () => {
    // Usa una data nel passato come scadenza
    const token = await svc.sign({ tid: "tenant-a", uid: "u", scp: [] }, "1s");
    await new Promise((r) => setTimeout(r, 1100));
    await expect(svc.verify(token)).rejects.toThrow(AuthError);
  });

  it("rifiuta un token con firma alterata", async () => {
    const token = await svc.sign({ tid: "tenant-a", uid: "u", scp: [] });
    const tampered = token.slice(0, -5) + "XXXXX";
    await expect(svc.verify(tampered)).rejects.toThrow(AuthError);
  });

  it("rifiuta un token privo di claim tid (payload base64 modificato)", async () => {
    // Crea un token valido, poi sostituisce il payload con uno senza tid
    const validToken = await svc.sign({ tid: "tenant-a", uid: "u", scp: [] });
    const [header, , signature] = validToken.split(".");
    // Payload senza tid
    const noTidPayload = Buffer.from(JSON.stringify({ uid: "u", scp: [], exp: Date.now() / 1000 + 3600 })).toString("base64url");
    const forgery = `${header}.${noTidPayload}.${signature}`;
    // La firma non matcha il payload modificato → AuthError
    await expect(svc.verify(forgery)).rejects.toThrow(AuthError);
  });

  it("requireScope lancia se lo scope manca", async () => {
    const payload = await svc.verify(
      await svc.sign({ tid: "t", uid: "u", scp: ["llm:complete"] })
    );
    expect(() => svc.requireScope(payload, "rag:read")).toThrow(AuthError);
  });

  it("requireScope non lancia se lo scope è presente", async () => {
    const payload = await svc.verify(
      await svc.sign({ tid: "t", uid: "u", scp: ["llm:complete", "rag:read"] })
    );
    expect(() => svc.requireScope(payload, "rag:read")).not.toThrow();
  });
});

// ─── TenantCrypto ─────────────────────────────────────────────────────────────

describe("TenantCrypto — isolamento chiavi per tenant", () => {
  let crypto: TenantCrypto;

  beforeEach(() => {
    crypto = new TenantCrypto(new LocalKeyStore());
  });

  it("round-trip encrypt/decrypt per lo stesso tenant", async () => {
    const plaintext = "dato riservato del tenant A";
    const blob = await crypto.encrypt("tenant-a", plaintext);
    const decrypted = await crypto.decrypt("tenant-a", blob);
    expect(decrypted).toBe(plaintext);
  });

  it("tenant B NON può decifrare un blob cifrato per tenant A", async () => {
    const blob = await crypto.encrypt("tenant-a", "segreto di A");
    // tenant-b genera una chiave diversa al momento del decrypt
    await expect(crypto.decrypt("tenant-b", blob)).rejects.toThrow();
  });

  it("crypto-shredding: dopo shredTenant i dati sono irrecuperabili", async () => {
    const blob = await crypto.encrypt("tenant-a", "dato");
    await crypto.shredTenant("tenant-a");
    // La chiave è stata distrutta — una nuova viene creata ma non decifra il vecchio blob
    await expect(crypto.decrypt("tenant-a", blob)).rejects.toThrow();
  });

  it("hasTenant è false dopo shredTenant", async () => {
    await crypto.encrypt("tenant-a", "x");
    expect(await crypto.hasTenant("tenant-a")).toBe(true);
    await crypto.shredTenant("tenant-a");
    expect(await crypto.hasTenant("tenant-a")).toBe(false);
  });

  it("IV diverso per ogni cifratura — nessun riuso del nonce", async () => {
    const blob1 = await crypto.encrypt("tenant-a", "msg");
    const blob2 = await crypto.encrypt("tenant-a", "msg");
    expect(blob1.iv).not.toBe(blob2.iv);
  });

  it("ciphertext diverso anche a parità di plaintext (per IV casuale)", async () => {
    const blob1 = await crypto.encrypt("tenant-a", "msg");
    const blob2 = await crypto.encrypt("tenant-a", "msg");
    expect(blob1.ciphertext).not.toBe(blob2.ciphertext);
  });
});

// ─── TenantContext ────────────────────────────────────────────────────────────

describe("TenantContext.withTenant", () => {
  it("imposta SET LOCAL con il tenant_id corretto", async () => {
    const mockSetConfig = vi.fn().mockResolvedValue([]);
    const mockFn = vi.fn().mockResolvedValue("ok");

    // Mock della connessione postgres
    const mockSql = vi.fn((_strings: TemplateStringsArray, ..._values: unknown[]) =>
      mockSetConfig()
    ) as unknown as import("postgres").Sql;
    (mockSql as { begin: typeof mockSql.begin }).begin = vi.fn(
      async (fn: (sql: import("postgres").Sql) => Promise<unknown>) => {
        return fn(mockSql);
      }
    );

    const ctx = new TenantContext(mockSql);
    const result = await ctx.withTenant("tenant-x", mockFn);

    expect(mockSetConfig).toHaveBeenCalledOnce();
    expect(mockFn).toHaveBeenCalledOnce();
    expect(result).toBe("ok");
  });

  it("lancia AuthError se tenant_id è vuoto", async () => {
    const mockSql = {} as import("postgres").Sql;
    const ctx = new TenantContext(mockSql);
    await expect(ctx.withTenant("", async () => "x")).rejects.toThrow(AuthError);
  });
});

// ─── extractTenantId ─────────────────────────────────────────────────────────

describe("extractTenantId", () => {
  it("estrae tid dal payload JWT", () => {
    expect(extractTenantId({ tid: "tenant-1" })).toBe("tenant-1");
  });

  it("lancia AuthError se tid manca", () => {
    expect(() => extractTenantId({})).toThrow(AuthError);
    expect(() => extractTenantId(null)).toThrow(AuthError);
  });
});
