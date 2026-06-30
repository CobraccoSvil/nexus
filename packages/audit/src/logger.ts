import { createHash } from "crypto";
import pino from "pino";
import type { LLMRequest, LLMResponse } from "@nexus/shared";

export interface AuditRecord {
  request_id: string;
  tenant_id: string;
  user_id: string;
  feature: string;
  sensitivity_tier: number;
  model_requested: string;
  model_used: string;
  provider_used: string;
  prompt_hash: string;        // SHA-256 del prompt — mai in chiaro nei log
  response_hash: string;      // SHA-256 della response
  input_tokens: number;
  output_tokens: number;
  latency_ms: number;
  finish_reason: string;
  redaction_applied: boolean;
  dlp_blocked: boolean;
  dlp_patterns: string[];
  timestamp: string;
  retention_until: string;
}

function sha256(text: string): string {
  return createHash("sha256").update(text, "utf8").digest("hex");
}

function messagesText(req: LLMRequest): string {
  return req.messages
    .map((m) => (typeof m.content === "string" ? m.content : JSON.stringify(m.content)))
    .join("\n");
}

export function buildAuditRecord(params: {
  req: LLMRequest;
  resp: LLMResponse;
  redactionApplied: boolean;
  dlpBlocked: boolean;
  dlpPatterns: string[];
  retentionDays?: number;
}): AuditRecord {
  const { req, resp, redactionApplied, dlpBlocked, dlpPatterns, retentionDays = 90 } = params;
  const now = new Date();
  const retentionUntil = new Date(now.getTime() + retentionDays * 86_400_000);

  return {
    request_id: req.metadata.request_id,
    tenant_id: req.metadata.tenant_id,
    user_id: req.metadata.user_id,
    feature: req.metadata.feature,
    sensitivity_tier: req.metadata.sensitivity_tier,
    model_requested: req.model,
    model_used: resp.model_used,
    provider_used: resp.provider_used,
    prompt_hash: sha256(messagesText(req)),
    response_hash: sha256(resp.content),
    input_tokens: resp.usage.input_tokens,
    output_tokens: resp.usage.output_tokens,
    latency_ms: resp.latency_ms,
    finish_reason: resp.finish_reason,
    redaction_applied: redactionApplied,
    dlp_blocked: dlpBlocked,
    dlp_patterns: dlpPatterns,
    timestamp: now.toISOString(),
    retention_until: retentionUntil.toISOString(),
  };
}

// Logger strutturato Pino — hook di audit per ogni chiamata LLM
export class AuditLogger {
  private log: pino.Logger;

  constructor(logLevel: string = "info") {
    const isDev = process.env.NODE_ENV !== "production";
    this.log = pino({
      level: logLevel,
      transport: isDev
        ? { target: "pino-pretty", options: { colorize: true, translateTime: "SYS:standard" } }
        : undefined,
    });
  }

  logAuditRecord(record: AuditRecord): void {
    // I campi hash garantiscono tracciabilità senza esporre payload in chiaro
    this.log.info(
      {
        audit: true,
        request_id: record.request_id,
        tenant_id: record.tenant_id,
        provider: record.provider_used,
        model: record.model_used,
        tier: record.sensitivity_tier,
        tokens: { in: record.input_tokens, out: record.output_tokens },
        latency_ms: record.latency_ms,
        redacted: record.redaction_applied,
        dlp_blocked: record.dlp_blocked,
        dlp_patterns: record.dlp_patterns,
      },
      "llm_call_audit"
    );
  }

  logDLPBlock(requestId: string, tenantId: string, patterns: string[]): void {
    this.log.warn(
      { audit: true, request_id: requestId, tenant_id: tenantId, patterns, dlp_blocked: true },
      "dlp_block"
    );
  }

  logAnomaly(requestId: string, tenantId: string, type: string, detail: string): void {
    this.log.warn(
      { audit: true, request_id: requestId, tenant_id: tenantId, anomaly_type: type, detail },
      "anomaly_detected"
    );
  }

  logError(requestId: string, err: Error): void {
    this.log.error({ request_id: requestId, error: err.message }, "llm_call_error");
  }
}
