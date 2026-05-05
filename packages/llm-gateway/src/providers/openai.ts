import OpenAI from "openai";
import type {
  LLMProvider,
  LLMRequest,
  LLMResponse,
  LLMStreamChunk,
  LLMToolCall,
  SensitivityTier,
} from "../types.js";
import { ProviderError } from "@nexus/shared";

function toOpenAIMessages(req: LLMRequest): OpenAI.Chat.ChatCompletionMessageParam[] {
  return req.messages.map((msg): OpenAI.Chat.ChatCompletionMessageParam => {
    if (msg.role === "tool") {
      return {
        role: "tool",
        tool_call_id: msg.tool_call_id ?? "",
        content: typeof msg.content === "string" ? msg.content : JSON.stringify(msg.content),
      };
    }

    if (msg.role === "assistant" && msg.tool_calls?.length) {
      return {
        role: "assistant",
        content: typeof msg.content === "string" ? msg.content : null,
        tool_calls: msg.tool_calls.map((tc) => ({
          id: tc.id,
          type: "function" as const,
          function: { name: tc.function.name, arguments: tc.function.arguments },
        })),
      };
    }

    return {
      role: msg.role as "system" | "user" | "assistant",
      content: typeof msg.content === "string" ? msg.content : JSON.stringify(msg.content),
    };
  });
}

function fromOpenAIResponse(
  resp: OpenAI.Chat.ChatCompletion,
  modelUsed: string,
  latencyMs: number
): LLMResponse {
  const choice = resp.choices[0];

  const tool_calls: LLMToolCall[] | undefined = choice.message.tool_calls?.map(
    (tc) => ({
      id: tc.id,
      type: "function" as const,
      function: { name: tc.function.name, arguments: tc.function.arguments },
    })
  );

  const finishReasonMap: Record<string, LLMResponse["finish_reason"]> = {
    stop: "stop",
    length: "length",
    tool_calls: "tool_calls",
    content_filter: "content_filter",
  };

  return {
    content: choice.message.content ?? "",
    tool_calls,
    usage: {
      input_tokens: resp.usage?.prompt_tokens ?? 0,
      output_tokens: resp.usage?.completion_tokens ?? 0,
    },
    model_used: modelUsed,
    provider_used: "openai",
    latency_ms: latencyMs,
    finish_reason: finishReasonMap[choice.finish_reason ?? "stop"] ?? "stop",
  };
}

export class OpenAIProvider implements LLMProvider {
  readonly name = "openai";
  readonly supports_tools = true;
  readonly supports_streaming = true;
  readonly max_context_tokens = 128_000;
  readonly tier_compatibility: SensitivityTier[] = [0, 1, 2];

  private client: OpenAI;

  constructor(private config: { api_key: string; base_url?: string }) {
    this.client = new OpenAI({
      apiKey: config.api_key,
      baseURL: config.base_url,
    });
  }

  async complete(req: LLMRequest): Promise<LLMResponse> {
    const start = Date.now();
    const messages = toOpenAIMessages(req);

    try {
      const resp = await this.client.chat.completions.create({
        model: req.model,
        messages,
        temperature: req.temperature,
        max_tokens: req.max_tokens,
        tools: req.tools?.map((t) => ({
          type: "function" as const,
          function: {
            name: t.function.name,
            description: t.function.description,
            parameters: t.function.parameters,
            strict: t.function.strict,
          },
        })),
        response_format:
          req.response_format === "json"
            ? { type: "json_object" as const }
            : req.response_format === "text" || req.response_format === undefined
            ? undefined
            : {
                type: "json_schema" as const,
                json_schema: {
                  name: "response",
                  schema: (req.response_format as { type: "json_schema"; schema: Record<string, unknown> }).schema,
                  strict: true,
                },
              },
      });

      return fromOpenAIResponse(resp, req.model, Date.now() - start);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      throw new ProviderError(`OpenAI error: ${msg}`, "openai");
    }
  }

  async *stream(req: LLMRequest): AsyncIterable<LLMStreamChunk> {
    const messages = toOpenAIMessages(req);

    try {
      const stream = await this.client.chat.completions.create({
        model: req.model,
        messages,
        temperature: req.temperature,
        max_tokens: req.max_tokens,
        stream: true,
        stream_options: { include_usage: true },
      });

      let usage: LLMResponse["usage"] | undefined;

      for await (const chunk of stream) {
        const delta = chunk.choices[0]?.delta;
        const finish_reason = chunk.choices[0]?.finish_reason;

        if (chunk.usage) {
          usage = {
            input_tokens: chunk.usage.prompt_tokens,
            output_tokens: chunk.usage.completion_tokens,
          };
        }

        if (delta?.tool_calls?.length) {
          const tc = delta.tool_calls[0];
          yield {
            delta: "",
            tool_call_delta: {
              index: tc.index,
              id: tc.id,
              function: tc.function,
            },
          };
          continue;
        }

        yield {
          delta: delta?.content ?? "",
          finish_reason: finish_reason as LLMResponse["finish_reason"] | undefined,
          usage: finish_reason ? usage : undefined,
        };
      }
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      throw new ProviderError(`OpenAI stream error: ${msg}`, "openai");
    }
  }

  async healthcheck(): Promise<boolean> {
    try {
      await this.client.models.list();
      return true;
    } catch {
      return false;
    }
  }
}
