// DeepSeek via OpenAI-compatible endpoint
//
// DeepSeek a volte genera tool calls in formato XML Anthropic-style
// (<tool_calls><invoke name="..."><parameter ...>...</invoke></tool_calls>)
// nel campo `content` della risposta, invece di usare il formato strutturato
// OpenAI `tool_calls`. Questo modulo intercetta e converte automaticamente.
import type { LLMProvider, LLMRequest, LLMResponse, LLMStreamChunk, LLMToolCall, SensitivityTier } from "../types.js";
import { OpenAIProvider } from "./openai.js";

/**
 * Regex per individuare il blocco XML <tool_calls>...</tool_calls> nel content.
 * Usa flag `s` (dotAll) per matchare anche newline nel body.
 */
const TOOL_CALLS_XML_RE = /<tool_calls>\s*([\s\S]*?)\s*<\/tool_calls>/;

/**
 * Regex per ogni singolo <invoke> dentro il blocco.
 * Cattura: name dell'invoke e il body (parametri).
 */
const INVOKE_RE = /<invoke\s+name=["']([^"']+)["']\s*>([\s\S]*?)<\/invoke>/g;

/**
 * Regex per ogni <parameter name="key" string="...">value</parameter>.
 * Supporta sia parametri con valore nel body che attributi type hint.
 */
const PARAM_RE = /<parameter\s+name=["']([^"']+)["'](?:\s+[^>]*)?>([^<]*)<\/parameter>/g;

/**
 * Tenta di parsare tool calls in formato XML dal content della risposta.
 * Ritorna un array di LLMToolCall se trovate, altrimenti null.
 */
function parseXmlToolCalls(content: string): LLMToolCall[] | null {
  const blockMatch = content.match(TOOL_CALLS_XML_RE);
  if (!blockMatch) return null;

  const block = blockMatch[1];
  const calls: LLMToolCall[] = [];
  let invokeMatch: RegExpExecArray | null;

  // Reset lastIndex per sicurezza (regex con flag g)
  INVOKE_RE.lastIndex = 0;
  while ((invokeMatch = INVOKE_RE.exec(block)) !== null) {
    const toolName = invokeMatch[1];
    const paramsBody = invokeMatch[2];
    const args: Record<string, unknown> = {};

    PARAM_RE.lastIndex = 0;
    let paramMatch: RegExpExecArray | null;
    while ((paramMatch = PARAM_RE.exec(paramsBody)) !== null) {
      const key = paramMatch[1];
      const rawValue = paramMatch[2].trim();

      // Prova a parsare come JSON (numeri, booleani, oggetti)
      try {
        args[key] = JSON.parse(rawValue);
      } catch {
        args[key] = rawValue;
      }
    }

    calls.push({
      id: `xmltc_${toolName}_${Date.now()}_${calls.length}`,
      type: "function",
      function: {
        name: toolName,
        arguments: JSON.stringify(args),
      },
    });
  }

  return calls.length > 0 ? calls : null;
}

/**
 * Rimuove il blocco <tool_calls>...</tool_calls> dal content,
 * lasciando eventuale testo prima/dopo.
 */
function stripXmlToolCalls(content: string): string {
  return content.replace(TOOL_CALLS_XML_RE, "").trim();
}

/**
 * Post-processa una risposta DeepSeek: se il content contiene XML tool calls
 * ma la risposta non ha tool_calls strutturate, le converte.
 */
function fixupResponse(resp: LLMResponse): LLMResponse {
  // Se ci sono gia' tool calls native, non toccare nulla
  if (resp.tool_calls && resp.tool_calls.length > 0) return resp;

  // Controlla se il content contiene XML tool calls
  if (!resp.content || !resp.content.includes("<tool_calls>")) return resp;

  const parsed = parseXmlToolCalls(resp.content);
  if (!parsed) return resp;

  return {
    ...resp,
    content: stripXmlToolCalls(resp.content),
    tool_calls: parsed,
    finish_reason: "tool_calls",
  };
}

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

  async complete(req: LLMRequest): Promise<LLMResponse> {
    const resp = await this.inner.complete(req);
    return fixupResponse({ ...resp, provider_used: "deepseek" });
  }

  async healthcheck(): Promise<boolean> {
    return this.inner.healthcheck();
  }

  async *stream(req: LLMRequest): AsyncGenerator<LLMStreamChunk> {
    // Per lo streaming, accumuliamo il content per detectare XML tool calls.
    // Se alla fine dello stream troviamo XML, emettiamo le tool calls.
    let accumulatedContent = "";
    let hasNativeToolCalls = false;
    const bufferedChunks: LLMStreamChunk[] = [];
    let lastUsage: LLMStreamChunk["usage"];
    let lastFinishReason: LLMStreamChunk["finish_reason"];

    for await (const chunk of this.inner.stream(req)) {
      if (chunk.tool_call_delta) {
        hasNativeToolCalls = true;
      }
      accumulatedContent += chunk.delta ?? "";
      bufferedChunks.push(chunk);

      if (chunk.usage) lastUsage = chunk.usage;
      if (chunk.finish_reason) lastFinishReason = chunk.finish_reason;
    }

    // Se ci sono tool calls native, yield diretto di tutti i chunk accumulati
    if (hasNativeToolCalls) {
      for (const chunk of bufferedChunks) {
        yield chunk;
      }
      return;
    }

    // Controlla se il contenuto accumulato ha XML tool calls
    if (accumulatedContent.includes("<tool_calls>")) {
      const parsed = parseXmlToolCalls(accumulatedContent);
      if (parsed) {
        // Emetti il content ripulito
        const cleanContent = stripXmlToolCalls(accumulatedContent);
        if (cleanContent) {
          yield { delta: cleanContent };
        }
        // Emetti ogni tool call come delta strutturato
        for (let i = 0; i < parsed.length; i++) {
          const tc = parsed[i];
          yield {
            delta: "",
            tool_call_delta: {
              index: i,
              id: tc.id,
              function: { name: tc.function.name, arguments: tc.function.arguments },
            },
          };
        }
        // Chiudi lo stream con finish_reason tool_calls
        yield {
          delta: "",
          finish_reason: "tool_calls",
          usage: lastUsage,
        };
        return;
      }
    }

    // Nessuna XML tool call: yield tutti i chunk originali
    for (const chunk of bufferedChunks) {
      yield chunk;
    }
  }
}
