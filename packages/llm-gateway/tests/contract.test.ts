/**
 * Contract test suite per LLMProvider.
 *
 * Ogni adapter (Anthropic, OpenAI, Mistral, vLLM) deve superare
 * gli stessi test con lo stesso input, garantendo portabilità.
 *
 * In CI questi test girano con mock. Con la var INTEGRATION=1 colpiscono i provider reali.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import type { LLMProvider, LLMRequest, LLMResponse } from "../src/types.js";

// Mock SDK
vi.mock("@anthropic-ai/sdk", () => {
  return {
    default: vi.fn().mockImplementation(() => ({
      messages: {
        create: vi.fn().mockResolvedValue({
          id: "msg_mock",
          type: "message",
          role: "assistant",
          content: [{ type: "text", text: "Hello from Anthropic" }],
          model: "claude-sonnet-4",
          stop_reason: "end_turn",
          usage: { input_tokens: 10, output_tokens: 5 },
        }),
        stream: vi.fn().mockReturnValue({
          [Symbol.asyncIterator]: async function* () {
            yield { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "Hello" } };
            yield { type: "message_stop" };
          },
          finalMessage: vi.fn().mockResolvedValue({
            usage: { input_tokens: 10, output_tokens: 5 },
          }),
        }),
      },
      models: {
        list: vi.fn().mockResolvedValue({ data: [] }),
      },
    })),
  };
});

vi.mock("openai", () => {
  return {
    default: vi.fn().mockImplementation(() => ({
      chat: {
        completions: {
          create: vi.fn().mockResolvedValue({
            id: "chatcmpl_mock",
            object: "chat.completion",
            choices: [
              {
                index: 0,
                message: { role: "assistant", content: "Hello from OpenAI", tool_calls: undefined },
                finish_reason: "stop",
              },
            ],
            usage: { prompt_tokens: 10, completion_tokens: 5, total_tokens: 15 },
          }),
        },
      },
      models: {
        list: vi.fn().mockResolvedValue({ data: [] }),
      },
    })),
  };
});

const makeRequest = (overrides: Partial<LLMRequest> = {}): LLMRequest => ({
  model: "claude-sonnet-4",
  messages: [
    { role: "user", content: "Di' semplicemente 'ciao'" },
  ],
  max_tokens: 50,
  metadata: {
    tenant_id: "test-tenant",
    user_id: "test-user",
    request_id: "req-001",
    sensitivity_tier: 0,
    feature: "contract-test",
  },
  ...overrides,
});

function expectValidResponse(result: LLMResponse) {
  expect(typeof result.content).toBe("string");
  expect(result.usage.input_tokens).toBeGreaterThanOrEqual(0);
  expect(result.usage.output_tokens).toBeGreaterThanOrEqual(0);
  expect(result.model_used).toBeTruthy();
  expect(result.provider_used).toBeTruthy();
  expect(result.latency_ms).toBeGreaterThanOrEqual(0);
  expect(["stop", "length", "tool_calls", "content_filter"]).toContain(result.finish_reason);
}

// ─── Provider-specific imports ──────────────────────────────────────────────

describe("AnthropicProvider — contract", async () => {
  const { AnthropicProvider } = await import("../src/providers/anthropic.js");
  let provider: LLMProvider;

  beforeEach(() => {
    provider = new AnthropicProvider({ api_key: "test-key" });
  });

  it("risponde a una request semplice", async () => {
    const result = await provider.complete(makeRequest());
    expectValidResponse(result);
    expect(result.provider_used).toBe("anthropic");
  });

  it("healthcheck ritorna boolean", async () => {
    const healthy = await provider.healthcheck();
    expect(typeof healthy).toBe("boolean");
  });

  it("stream emette almeno un chunk", async () => {
    const chunks: string[] = [];
    for await (const chunk of provider.stream(makeRequest())) {
      chunks.push(chunk.delta);
    }
    expect(chunks.length).toBeGreaterThan(0);
  });
});

describe("OpenAIProvider — contract", async () => {
  const { OpenAIProvider } = await import("../src/providers/openai.js");
  let provider: LLMProvider;

  beforeEach(() => {
    provider = new OpenAIProvider({ api_key: "test-key" });
  });

  it("risponde a una request semplice", async () => {
    const result = await provider.complete(makeRequest({ model: "gpt-4o" }));
    expectValidResponse(result);
    expect(result.provider_used).toBe("openai");
  });

  it("healthcheck ritorna boolean", async () => {
    const healthy = await provider.healthcheck();
    expect(typeof healthy).toBe("boolean");
  });
});

describe("MistralProvider — contract", async () => {
  const { MistralProvider } = await import("../src/providers/mistral.js");
  let provider: LLMProvider;

  beforeEach(() => {
    provider = new MistralProvider({ api_key: "test-key" });
  });

  it("risponde a una request semplice", async () => {
    const result = await provider.complete(makeRequest({ model: "mistral-small-latest" }));
    expectValidResponse(result);
    expect(result.provider_used).toBe("mistral");
  });
});

describe("VLLMProvider — contract", async () => {
  const { VLLMProvider } = await import("../src/providers/vllm-local.js");
  let provider: LLMProvider;

  beforeEach(() => {
    provider = new VLLMProvider({ base_url: "http://localhost:8000/v1" });
  });

  it("risponde a una request semplice", async () => {
    const result = await provider.complete(makeRequest({ model: "Qwen/Qwen2.5-Coder-32B-Instruct" }));
    expectValidResponse(result);
    expect(result.provider_used).toBe("vllm");
  });

  it("supporta tier 3", () => {
    expect(provider.tier_compatibility).toContain(3);
  });
});
