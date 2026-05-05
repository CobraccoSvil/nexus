// Google Gemini via OpenAI-compatible endpoint
import type { LLMProvider, LLMRequest, LLMResponse, LLMStreamChunk, SensitivityTier } from "../types.js";
import { OpenAIProvider } from "./openai.js";

export class GoogleProvider implements LLMProvider {
  readonly name = "google";
  readonly supports_tools = true;
  readonly supports_streaming = true;
  readonly max_context_tokens = 1_000_000;
  readonly tier_compatibility: SensitivityTier[] = [0, 1, 2];

  private inner: OpenAIProvider;

  constructor(config: { api_key: string; base_url?: string }) {
    this.inner = new OpenAIProvider({
      api_key: config.api_key,
      base_url: config.base_url ?? `https://generativelanguage.googleapis.com/v1beta/openai`,
    });
  }

  complete(req: LLMRequest): Promise<LLMResponse> {
    return this.inner.complete(req).then((resp) => ({ ...resp, provider_used: "google" }));
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
