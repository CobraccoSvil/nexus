import Anthropic from "@anthropic-ai/sdk";
import type {
  LLMProvider,
  LLMRequest,
  LLMResponse,
  LLMStreamChunk,
  LLMToolCall,
  SensitivityTier,
} from "../types.js";
import { ProviderError } from "@nexus/shared";

function toAnthropicMessages(req: LLMRequest): {
  system?: string;
  messages: Anthropic.MessageParam[];
} {
  let system: string | undefined;
  const messages: Anthropic.MessageParam[] = [];

  for (const msg of req.messages) {
    if (msg.role === "system") {
      system = typeof msg.content === "string" ? msg.content : JSON.stringify(msg.content);
      continue;
    }

    if (msg.role === "tool") {
      messages.push({
        role: "user",
        content: [
          {
            type: "tool_result",
            tool_use_id: msg.tool_call_id ?? "",
            content: typeof msg.content === "string" ? msg.content : JSON.stringify(msg.content),
          },
        ],
      });
      continue;
    }

    if (msg.role === "assistant" && msg.tool_calls?.length) {
      messages.push({
        role: "assistant",
        content: [
          ...(msg.content ? [{ type: "text" as const, text: typeof msg.content === "string" ? msg.content : "" }] : []),
          ...msg.tool_calls.map((tc) => ({
            type: "tool_use" as const,
            id: tc.id,
            name: tc.function.name,
            input: JSON.parse(tc.function.arguments || "{}"),
          })),
        ],
      });
      continue;
    }

    messages.push({
      role: msg.role as "user" | "assistant",
      content: typeof msg.content === "string" ? msg.content : JSON.stringify(msg.content),
    });
  }

  return { system, messages };
}

function fromAnthropicResponse(
  resp: Anthropic.Message,
  modelUsed: string,
  latencyMs: number
): LLMResponse {
  const textContent = resp.content
    .filter((b): b is Anthropic.TextBlock => b.type === "text")
    .map((b) => b.text)
    .join("");

  const toolUseBlocks = resp.content.filter(
    (b): b is Anthropic.ToolUseBlock => b.type === "tool_use"
  );

  const tool_calls: LLMToolCall[] | undefined = toolUseBlocks.length
    ? toolUseBlocks.map((b) => ({
        id: b.id,
        type: "function" as const,
        function: { name: b.name, arguments: JSON.stringify(b.input) },
      }))
    : undefined;

  const finishReasonMap: Record<string, LLMResponse["finish_reason"]> = {
    end_turn: "stop",
    max_tokens: "length",
    tool_use: "tool_calls",
  };

  return {
    content: textContent,
    tool_calls,
    usage: {
      input_tokens: resp.usage.input_tokens,
      output_tokens: resp.usage.output_tokens,
    },
    model_used: modelUsed,
    provider_used: "anthropic",
    latency_ms: latencyMs,
    finish_reason: finishReasonMap[resp.stop_reason ?? "end_turn"] ?? "stop",
  };
}

/** Rileva se un messaggio di errore indica un problema di crediti/billing. */
function isBillingError(msg: string): boolean {
  const lower = msg.toLowerCase();
  return (
    (lower.includes("credit balance") && lower.includes("too low")) ||
    lower.includes("insufficient_quota") ||
    lower.includes("exceeded your current quota") ||
    lower.includes("plans & billing") ||
    lower.includes("upgrade or purchase credits") ||
    lower.includes("payment required") ||
    lower.includes("billing required")
  );
}

export class AnthropicProvider implements LLMProvider {
  readonly name = "anthropic";
  readonly supports_tools = true;
  readonly supports_streaming = true;
  readonly max_context_tokens = 200_000;
  readonly tier_compatibility: SensitivityTier[] = [0, 1, 2];

  private client: Anthropic;
  /** Timestamp dell'ultimo errore di billing rilevato; null = nessuno. */
  billingError: string | null = null;

  constructor(private config: { api_key: string; base_url?: string }) {
    this.client = new Anthropic({
      apiKey: config.api_key,
      baseURL: config.base_url,
    });
  }

  async complete(req: LLMRequest): Promise<LLMResponse> {
    const start = Date.now();
    const { system, messages } = toAnthropicMessages(req);

    try {
      const resp = await this.client.messages.create({
        model: req.model,
        max_tokens: req.max_tokens ?? 4096,
        temperature: req.temperature,
        system,
        messages,
        tools:
          req.tools?.map((t) => ({
            name: t.function.name,
            description: t.function.description ?? "",
            input_schema: t.function.parameters as Anthropic.Tool["input_schema"],
          })) ?? undefined,
      });

      return fromAnthropicResponse(resp, req.model, Date.now() - start);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      if (isBillingError(msg)) {
        this.billingError = msg;
      }
      throw new ProviderError(`Anthropic error: ${msg}`, "anthropic");
    }
  }

  async *stream(req: LLMRequest): AsyncIterable<LLMStreamChunk> {
    const { system, messages } = toAnthropicMessages(req);

    try {
      const stream = await this.client.messages.stream({
        model: req.model,
        max_tokens: req.max_tokens ?? 4096,
        temperature: req.temperature,
        system,
        messages,
      });

      for await (const event of stream) {
        if (
          event.type === "content_block_delta" &&
          event.delta.type === "text_delta"
        ) {
          yield { delta: event.delta.text };
        }
        if (event.type === "message_stop") {
          const final = await stream.finalMessage();
          yield {
            delta: "",
            finish_reason: "stop",
            usage: {
              input_tokens: final.usage.input_tokens,
              output_tokens: final.usage.output_tokens,
            },
          };
        }
      }
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      if (isBillingError(msg)) {
        this.billingError = msg;
      }
      throw new ProviderError(`Anthropic stream error: ${msg}`, "anthropic");
    }
  }

  async healthcheck(): Promise<boolean> {
    // Se è stato rilevato un errore di billing nelle chiamate precedenti, ritorna false
    // così lo stato viene propagato al frontend senza consumare crediti.
    if (this.billingError) {
      return false;
    }
    try {
      await this.client.models.list();
      return true;
    } catch {
      return false;
    }
  }
}
