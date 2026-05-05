import type { LLMMessage, LLMRequest, LLMResponse } from "../types.js";
import { SecretScanner } from "../router/secret-scanner.js";
import { PresidioClient } from "../router/presidio-client.js";
import { CodeAnonymizer } from "./code-anonymizer.js";
import { PathPolicy } from "./path-policy.js";
import { RedactionMap } from "./redaction-map.js";
import { RedactionError } from "@nexus/shared";

export interface RedactionOptions {
  presidioGrpcUrl: string;
  strictMode: boolean;
  ttlMs?: number;
  pathPolicy?: { whitelist?: string[]; blacklist?: string[] };
}

export interface RedactionResult {
  messages: LLMMessage[];
  map: RedactionMap;
  stats: {
    secrets_found: number;
    pii_found: number;
    code_anonymized: number;
    types: string[];
  };
}

function extractFilePath(content: string): string | null {
  // Cerca pattern tipo "File: path/to/file.ts" o "```typescript\n// path/to/file.ts"
  const match =
    /(?:File|file):\s*([\w./\-]+\.\w+)/.exec(content) ??
    /^\/\/\s*([\w./\-]+\.\w+)/m.exec(content);
  return match?.[1] ?? null;
}

function redactContent(
  content: string,
  scanner: SecretScanner,
  anonymizer: CodeAnonymizer,
  pathPolicy: PathPolicy,
  map: RedactionMap
): { text: string; stats: { secrets: number; code: number; types: string[] } } {
  // Controlla path policy se il messaggio riferisce un file
  const filePath = extractFilePath(content);
  if (filePath) {
    const policy = pathPolicy.checkPath(filePath);
    if (policy === "blocked") {
      throw new RedactionError(
        `File "${filePath}" è in blacklist e non può essere inviato a provider esterni`,
        { filePath }
      );
    }
    if (policy === "whitelisted") {
      return { text: content, stats: { secrets: 0, code: 0, types: [] } };
    }
  }

  // Secret scanner — redact diretto
  const { redacted, count: secretCount } = scanner.redact(content);

  // Code anonymizer — usa la redaction map per reidratazione
  const { text, count: codeCount, types } = anonymizer.anonymize(redacted, map);

  return {
    text,
    stats: { secrets: secretCount, code: codeCount, types },
  };
}

export class RedactionPipeline {
  private scanner: SecretScanner;
  private presidio: PresidioClient;
  private anonymizer: CodeAnonymizer;
  private pathPolicy: PathPolicy;

  constructor(private opts: RedactionOptions) {
    this.scanner = new SecretScanner();
    this.presidio = new PresidioClient(opts.presidioGrpcUrl);
    this.anonymizer = new CodeAnonymizer();
    this.pathPolicy = new PathPolicy(opts.pathPolicy);
  }

  // Pre-flight: redige i messaggi prima di inviarli al provider
  async redact(req: LLMRequest): Promise<RedactionResult> {
    const map = new RedactionMap(req.metadata.request_id, this.opts.ttlMs);
    const stats = { secrets_found: 0, pii_found: 0, code_anonymized: 0, types: [] as string[] };

    const redactedMessages: LLMMessage[] = [];

    for (const msg of req.messages) {
      // Messaggi tool o system con poco testo: solo secret scanner
      if (msg.role === "system" || msg.role === "tool") {
        const content = typeof msg.content === "string" ? msg.content : JSON.stringify(msg.content);
        const { redacted, count } = this.scanner.redact(content);
        stats.secrets_found += count;
        redactedMessages.push({ ...msg, content: redacted });
        continue;
      }

      const rawContent =
        typeof msg.content === "string" ? msg.content : JSON.stringify(msg.content);

      // Redaction completa (secret + code anonymizer)
      const { text, stats: s } = redactContent(
        rawContent,
        this.scanner,
        this.anonymizer,
        this.pathPolicy,
        map
      );
      stats.secrets_found += s.secrets;
      stats.code_anonymized += s.code;
      for (const t of s.types) {
        if (!stats.types.includes(t)) stats.types.push(t);
      }

      // Presidio PII detection + store in map
      const presidioResult = await this.presidio.analyze(text);
      if (presidioResult.has_pii) {
        stats.pii_found += presidioResult.entities.length;
        // In strict mode le entità PII trovate da Presidio vengono sostituite
        let finalText = text;
        if (this.opts.strictMode) {
          for (const entity of presidioResult.entities.sort((a, b) => b.start - a.start)) {
            const original = finalText.slice(entity.start, entity.end);
            const placeholder = map.store(original, entity.entity_type.toLowerCase());
            finalText =
              finalText.slice(0, entity.start) + placeholder + finalText.slice(entity.end);
          }
        }
        redactedMessages.push({ ...msg, content: finalText });
      } else {
        redactedMessages.push({ ...msg, content: text });
      }
    }

    return { messages: redactedMessages, map, stats };
  }

  // Post-flight: reidrata la risposta del provider (potrebbe contenere i placeholder)
  rehydrate(response: LLMResponse, map: RedactionMap): LLMResponse {
    return {
      ...response,
      content: map.rehydrate(response.content),
    };
  }
}
