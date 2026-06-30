import type { LLMRequest, LLMResponse } from "@nexus/shared";
import type { AuditRecord } from "./logger.js";

interface LangfuseConfig {
  host: string;
  secretKey: string;
  publicKey?: string;
  enabled: boolean;
}

// Lazy import per evitare errori se Langfuse non è installato
let LangfuseClass: typeof import("langfuse").Langfuse | null = null;

async function getLangfuse(): Promise<typeof import("langfuse").Langfuse | null> {
  if (LangfuseClass) return LangfuseClass;
  try {
    const mod = await import("langfuse");
    LangfuseClass = mod.Langfuse;
    return LangfuseClass;
  } catch {
    return null;
  }
}

export class LangfuseTracer {
  private client: InstanceType<typeof import("langfuse").Langfuse> | null = null;
  private initialized = false;

  constructor(private config: LangfuseConfig) {}

  private async init(): Promise<void> {
    if (this.initialized) return;
    this.initialized = true;

    if (!this.config.enabled) return;

    const Langfuse = await getLangfuse();
    if (!Langfuse) return;

    this.client = new Langfuse({
      baseUrl: this.config.host,
      secretKey: this.config.secretKey,
      publicKey: this.config.publicKey ?? "pk-lf-placeholder",
      flushAt: 20,
      flushInterval: 5000,
    });
  }

  async traceCall(params: {
    req: LLMRequest;
    resp: LLMResponse;
    record: AuditRecord;
    sessionId?: string;
  }): Promise<void> {
    await this.init();
    if (!this.client) return;

    const { req, resp, record, sessionId } = params;

    const trace = this.client.trace({
      id: req.metadata.request_id,
      name: `llm_call:${req.metadata.feature}`,
      userId: req.metadata.user_id,
      sessionId: sessionId ?? req.metadata.tenant_id,
      metadata: {
        tenant_id: req.metadata.tenant_id,
        sensitivity_tier: req.metadata.sensitivity_tier,
        redaction_applied: record.redaction_applied,
        dlp_blocked: record.dlp_blocked,
      },
      tags: [`tier:${req.metadata.sensitivity_tier}`, `provider:${resp.provider_used}`],
    });

    trace.generation({
      name: "llm_generation",
      model: resp.model_used,
      modelParameters: {
        ...(req.temperature !== undefined && { temperature: req.temperature }),
        ...(req.max_tokens !== undefined && { max_tokens: req.max_tokens }),
      },
      // I messaggi non sono mai inviati a Langfuse in chiaro — solo hash e metadati
      input: { prompt_hash: record.prompt_hash, message_count: req.messages.length },
      output: { response_hash: record.response_hash, finish_reason: resp.finish_reason },
      usage: {
        input: resp.usage.input_tokens,
        output: resp.usage.output_tokens,
        total: resp.usage.input_tokens + resp.usage.output_tokens,
        unit: "TOKENS",
      },
      startTime: new Date(Date.now() - resp.latency_ms),
      endTime: new Date(),
    });
  }

  async flush(): Promise<void> {
    await this.client?.flushAsync();
  }

  async shutdown(): Promise<void> {
    await this.flush();
    await this.client?.shutdownAsync();
  }
}
