// Mistral via OpenAI-compatible endpoint — thin wrapper su OpenAIProvider
import type {
  LLMProvider,
  LLMRequest,
  LLMResponse,
  LLMStreamChunk,
  SensitivityTier,
} from "../types.js";
import { OpenAIProvider } from "./openai.js";

export class MistralProvider implements LLMProvider {
  readonly name = "mistral";
  readonly supports_tools = true;
  readonly supports_streaming = true;
  readonly max_context_tokens = 128_000;
  readonly tier_compatibility: SensitivityTier[] = [0, 1, 2];

  private inner: OpenAIProvider;

  constructor(config: { api_key: string; base_url?: string }) {
    this.inner = new OpenAIProvider({
      api_key: config.api_key,
      base_url: config.base_url ?? "https://api.mistral.ai/v1",
    });
  }

  complete(req: LLMRequest): Promise<LLMResponse> {
    return this.inner.complete(req).then((resp) => ({
      ...resp,
      provider_used: "mistral",
    }));
  }

  async *stream(req: LLMRequest): AsyncIterable<LLMStreamChunk> {
    yield* this.inner.stream(req);
  }

  healthcheck(): Promise<boolean> {
    return this.inner.healthcheck();
  }
}
