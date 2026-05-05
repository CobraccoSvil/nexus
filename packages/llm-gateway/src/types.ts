// Re-export dei tipi LLM condivisi da @nexus/shared
export type {
  SensitivityTier,
  LLMContentBlock,
  LLMToolCall,
  LLMToolDefinition,
  LLMMessage,
  RequestMetadata,
  LLMRequest,
  LLMResponse,
  LLMStreamChunk,
} from "@nexus/shared";

export interface LLMProvider {
  readonly name: string;
  readonly supports_tools: boolean;
  readonly supports_streaming: boolean;
  readonly max_context_tokens: number;
  readonly tier_compatibility: import("@nexus/shared").SensitivityTier[];

  complete(req: import("@nexus/shared").LLMRequest): Promise<import("@nexus/shared").LLMResponse>;
  stream(req: import("@nexus/shared").LLMRequest): AsyncIterable<import("@nexus/shared").LLMStreamChunk>;
  healthcheck(): Promise<boolean>;
}

export interface ModelAliasEntry {
  cloud_primary: string | null;
  cloud_secondary: string | null;
  onprem: string | null;
  min_tier: import("@nexus/shared").SensitivityTier;
  max_tier: import("@nexus/shared").SensitivityTier;
}

export interface ModelAliases {
  aliases: Record<string, ModelAliasEntry>;
}

export interface ProviderStatus {
  name: string;
  healthy: boolean;
  last_check: Date;
  last_error?: string;
  /** Messaggio di errore di billing (crediti esauriti). Presente solo se rilevato. */
  billing_error?: string;
}

export type ProviderName = "anthropic" | "openai" | "mistral" | "vllm" | "deepseek" | "google";
