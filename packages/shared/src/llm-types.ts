// Nexus LLM Gateway — core types shared across packages
// Modeled after OpenAI Chat Completions (lingua franca)

export type SensitivityTier = 0 | 1 | 2 | 3;

export interface LLMContentBlock {
  type: "text" | "image_url" | "tool_result";
  text?: string;
  image_url?: { url: string; detail?: "low" | "high" | "auto" };
  tool_use_id?: string;
  content?: string;
}

export interface LLMToolCall {
  id: string;
  type: "function";
  function: {
    name: string;
    arguments: string;
  };
}

export interface LLMToolDefinition {
  type: "function";
  function: {
    name: string;
    description?: string;
    parameters: Record<string, unknown>;
    strict?: boolean;
  };
}

export interface LLMMessage {
  role: "system" | "user" | "assistant" | "tool";
  content: string | LLMContentBlock[];
  tool_call_id?: string;
  tool_calls?: LLMToolCall[];
  name?: string;
}

export interface RequestMetadata {
  tenant_id: string;
  user_id: string;
  request_id: string;
  sensitivity_tier: SensitivityTier;
  feature: string;
}

export interface LLMRequest {
  model: string;
  messages: LLMMessage[];
  temperature?: number;
  max_tokens?: number;
  tools?: LLMToolDefinition[];
  response_format?: "text" | "json" | { type: "json_schema"; schema: object };
  stream?: boolean;
  metadata: RequestMetadata;
}

export interface LLMResponse {
  content: string;
  tool_calls?: LLMToolCall[];
  usage: {
    input_tokens: number;
    output_tokens: number;
  };
  model_used: string;
  provider_used: string;
  latency_ms: number;
  finish_reason: "stop" | "length" | "tool_calls" | "content_filter";
  /**
   * Presente quando la richiesta è stata re-instradata automaticamente
   * verso un provider locale per motivi di privacy (tier sensitivity block).
   */
  privacy_rerouted?: {
    /** Provider locale usato al posto del provider cloud bloccato */
    provider: string;
    /** Tier di sensibilità che ha attivato il blocco */
    blocked_tier: number;
    /** Messaggio descrittivo da mostrare all'utente */
    reason: string;
  };
}

export interface LLMStreamChunk {
  delta: string;
  tool_call_delta?: {
    index: number;
    id?: string;
    function?: {
      name?: string;
      arguments?: string;
    };
  };
  finish_reason?: LLMResponse["finish_reason"];
  usage?: LLMResponse["usage"];
  /**
   * Opzionale: quando disponibile, permette telemetria affidabile lato gateway
   * anche per lo streaming (provider/modello usati).
   */
  provider_used?: string;
  model_used?: string;
}
