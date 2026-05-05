# ADR 0001: Provider Abstraction Layer

**Status**: Accepted

**Date**: 2026-04-17

## Context

The Nexus LLM system needs to support multiple provider backends (Anthropic, OpenAI, Mistral) initially, and transition to self-hosted vLLM later without requiring code changes in the application layer.

To achieve this portability, we must prevent provider-specific SDK calls from leaking into business logic. Instead, all provider interactions must pass through a unified abstraction.

## Decision

We will implement a **Provider Abstraction Layer** with:

1. **Unified Interface** (`LLMProvider`)
   - All providers (cloud and self-hosted) implement the same interface
   - Request/response types are provider-agnostic (modeled after OpenAI Chat Completions API)
   - Streaming, tool calling, and response format options are normalized

2. **Model Alias Resolution**
   - Application code references logical model names (e.g., `"coder-large"`)
   - Runtime resolves aliases to provider-specific models via `config/model-aliases.yaml`
   - This decouples application code from provider-specific model identifiers

3. **LLMGateway Singleton**
   - Single entry point for all LLM calls
   - Manages provider registry, health checks, fallback chain, rate limiting
   - All cross-cutting concerns (redaction, audit, DLP) layer into the gateway

4. **Configuration-Driven Dispatch**
   - Provider selection, tier→provider mapping, feature flags loaded at runtime
   - No recompilation needed to switch between cloud/hybrid/onprem profiles

## Consequences

### Positive
- ✅ Porting to on-premise vLLM requires only config/adapter changes, no business logic rewrites
- ✅ Adding a new provider is localized to `packages/llm-gateway/src/providers/`
- ✅ Testing is centralized: once LLMProvider contract is tested, new adapters inherit the suite
- ✅ Fallback chain and rate limiting are implemented once, reused across all providers

### Negative
- ⚠️ One additional abstraction layer adds ~5-10% latency to direct SDK calls (negligible for network I/O)
- ⚠️ Some provider-specific features (e.g., Anthropic's extended thinking) require capability discovery in the interface
- ⚠️ Type-safe provider config needs careful zod schema management as adapters grow

## Implementation Notes

- Start with Anthropic, OpenAI, Mistral adapters in Phase 1
- Add vLLM adapter in Phase 7 (but all plumbing ready from Phase 0)
- Health checks run periodically; unhealthy providers are marked and excluded from routing
- Streaming support via AsyncIterable<LLMStreamChunk> to support all backend varieties

## References

- Plan section 4: "Provider Abstraction Layer"
- Phase 1: LLM Gateway implementation
- Phase 7: Portability to on-premise
