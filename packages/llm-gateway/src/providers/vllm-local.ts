// vLLM — OpenAI-compatible self-hosted endpoint
// Pronto dalla Fase 0; attivato in Fase 7
import type {
  LLMProvider,
  LLMRequest,
  LLMResponse,
  LLMStreamChunk,
  SensitivityTier,
} from "../types.js";
import { OpenAIProvider } from "./openai.js";

export class VLLMProvider implements LLMProvider {
  readonly name = "vllm";
  readonly supports_tools = true;
  readonly supports_streaming = true;
  readonly max_context_tokens: number;
  readonly tier_compatibility: SensitivityTier[] = [0, 1, 2, 3];

  private inner: OpenAIProvider;

  constructor(config: {
    base_url: string;
    api_key?: string;
    max_context_tokens?: number;
  }) {
    this.max_context_tokens = config.max_context_tokens ?? 32_768;
    this.inner = new OpenAIProvider({
      api_key: config.api_key ?? "no-key",
      base_url: config.base_url,
    });
  }

  complete(req: LLMRequest): Promise<LLMResponse> {
    return this.inner.complete(req).then((resp) => ({
      ...resp,
      provider_used: "vllm",
    }));
  }

  async *stream(req: LLMRequest): AsyncIterable<LLMStreamChunk> {
    yield* this.inner.stream(req);
  }

  healthcheck(): Promise<boolean> {
    return this.inner.healthcheck();
  }
}
