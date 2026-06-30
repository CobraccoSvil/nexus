import { describe, it, expect, vi } from "vitest";
import { buildAuditRecord } from "../src/logger.js";
import type { LLMRequest, LLMResponse } from "@nexus/shared";

const mockReq: LLMRequest = {
  model: "coder-large",
  messages: [{ role: "user", content: "Hello" }],
  metadata: {
    tenant_id: "tenant-1",
    user_id: "user-1",
    request_id: "req-abc",
    sensitivity_tier: 1,
    feature: "chat",
  },
};

const mockResp: LLMResponse = {
  content: "Hi there!",
  usage: { input_tokens: 10, output_tokens: 5 },
  model_used: "claude-sonnet-4",
  provider_used: "anthropic",
  latency_ms: 200,
  finish_reason: "stop",
};

describe("buildAuditRecord", () => {
  it("builds a record with correct fields", () => {
    const record = buildAuditRecord({
      req: mockReq,
      resp: mockResp,
      redactionApplied: false,
      dlpBlocked: false,
      dlpPatterns: [],
    });

    expect(record.request_id).toBe("req-abc");
    expect(record.tenant_id).toBe("tenant-1");
    expect(record.provider_used).toBe("anthropic");
    expect(record.input_tokens).toBe(10);
    expect(record.output_tokens).toBe(5);
    expect(record.latency_ms).toBe(200);
    expect(record.redaction_applied).toBe(false);
    expect(record.dlp_blocked).toBe(false);
  });

  it("never stores prompt or response in plain text", () => {
    const record = buildAuditRecord({
      req: mockReq,
      resp: mockResp,
      redactionApplied: false,
      dlpBlocked: false,
      dlpPatterns: [],
    });

    const serialized = JSON.stringify(record);
    expect(serialized).not.toContain("Hello");
    expect(serialized).not.toContain("Hi there!");
    // SHA-256 hash is a 64-char hex string
    expect(record.prompt_hash).toMatch(/^[a-f0-9]{64}$/);
    expect(record.response_hash).toMatch(/^[a-f0-9]{64}$/);
  });

  it("sets retention_until 90 days in the future by default", () => {
    const record = buildAuditRecord({
      req: mockReq,
      resp: mockResp,
      redactionApplied: false,
      dlpBlocked: false,
      dlpPatterns: [],
    });

    const now = Date.now();
    const retention = new Date(record.retention_until).getTime();
    const diffDays = (retention - now) / 86_400_000;
    expect(diffDays).toBeGreaterThan(89);
    expect(diffDays).toBeLessThan(91);
  });

  it("accepts custom retention days", () => {
    const record = buildAuditRecord({
      req: mockReq,
      resp: mockResp,
      redactionApplied: false,
      dlpBlocked: false,
      dlpPatterns: [],
      retentionDays: 30,
    });

    const now = Date.now();
    const diffDays = (new Date(record.retention_until).getTime() - now) / 86_400_000;
    expect(diffDays).toBeCloseTo(30, 0);
  });
});
