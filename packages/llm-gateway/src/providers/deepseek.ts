// DeepSeek via OpenAI-compatible endpoint
import type { LLMProvider, LLMRequest, LLMResponse, LLMStreamChunk, SensitivityTier } from "../types.js";
import { OpenAIProvider } from "./openai.js";

export class DeepSeekProvider implements LLMProvider {
  readonly name = "deepseek";
  readonly supports_tools = true;
  readonly supports_streaming = true;
  readonly max_context_tokens = 65_536;
  readonly tier_compatibility: SensitivityTier[] = [0, 1, 2];

  private inner: OpenAIProvider;

  constructor(config: { api_key: string; base_url?: string }) {
    this.inner = new OpenAIProvider({
      api_key: config.api_key,
      base_url: config.base_url ?? "https://api.deepseek.com",
    });
  }

  complete(req: LLMRequest): Promise<LLMResponse> {
    return this.inner.complete(req).then((resp) => ({ ...resp, provider_used: "deepseek" }));
  }

  async healthcheck(): Promise<boolean> {
    return this.inner.healthcheck();
  }

  async *stream(req: LLMRequest): AsyncGenerator<LLMStreamChunk> {
    for await (const chunk of this.inner.stream(req)) {
      yield chunk;
    }
  }
}
