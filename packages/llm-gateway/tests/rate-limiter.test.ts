import { describe, it, expect } from "vitest";
import { RateLimiter } from "../src/router/rate-limiter.js";
import { RateLimitError } from "@nexus/shared";

describe("RateLimiter", () => {
  it("permette richieste entro il limite", () => {
    const rl = new RateLimiter({
      perTenant: { requests: 5, windowMs: 60_000 },
      perProvider: { requests: 100, windowMs: 60_000 },
    });

    expect(() => {
      for (let i = 0; i < 5; i++) rl.checkTenant("tenant-a");
    }).not.toThrow();
  });

  it("blocca richieste oltre il limite", () => {
    const rl = new RateLimiter({
      perTenant: { requests: 3, windowMs: 60_000 },
      perProvider: { requests: 100, windowMs: 60_000 },
    });

    for (let i = 0; i < 3; i++) rl.checkTenant("tenant-b");

    expect(() => rl.checkTenant("tenant-b")).toThrow(RateLimitError);
  });

  it("tenant diversi hanno contatori indipendenti", () => {
    const rl = new RateLimiter({
      perTenant: { requests: 2, windowMs: 60_000 },
      perProvider: { requests: 100, windowMs: 60_000 },
    });

    expect(() => {
      rl.checkTenant("tenant-x");
      rl.checkTenant("tenant-y");
      rl.checkTenant("tenant-x"); // ancora ok, è il secondo di tenant-x
    }).not.toThrow();

    expect(() => rl.checkTenant("tenant-x")).toThrow(RateLimitError);
    expect(() => rl.checkTenant("tenant-y")).not.toThrow();
  });

  it("RateLimitError contiene retryAfterMs", () => {
    const rl = new RateLimiter({
      perTenant: { requests: 1, windowMs: 10_000 },
      perProvider: { requests: 100, windowMs: 60_000 },
    });

    rl.checkTenant("tenant-z");

    try {
      rl.checkTenant("tenant-z");
      expect.fail("Doveva lanciare RateLimitError");
    } catch (err) {
      expect(err).toBeInstanceOf(RateLimitError);
      expect((err as RateLimitError).retryAfterMs).toBeGreaterThan(0);
    }
  });

  it("provider rate limit funziona separatamente dal tenant", () => {
    const rl = new RateLimiter({
      perTenant: { requests: 100, windowMs: 60_000 },
      perProvider: { requests: 2, windowMs: 60_000 },
    });

    rl.checkProvider("anthropic");
    rl.checkProvider("anthropic");
    expect(() => rl.checkProvider("anthropic")).toThrow(RateLimitError);
    expect(() => rl.checkProvider("openai")).not.toThrow();
  });
});
