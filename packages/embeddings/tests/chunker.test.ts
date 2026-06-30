import { describe, it, expect } from "vitest";
import { Chunker } from "../src/chunker.js";

describe("Chunker — markdown", () => {
  const chunker = new Chunker({ maxChunkSize: 500, overlap: 50, minChunkSize: 30 });

  const MARKDOWN = `# Architettura

Il sistema usa un LLM Gateway per astrarre i provider.

## Provider Cloud

Anthropic, OpenAI e Mistral sono supportati tramite adapter.

### Anthropic

Usa il modello claude-sonnet-4 per task complessi.

### OpenAI

Usa gpt-4o come provider secondario.

## On-Premise

vLLM espone un'API OpenAI-compatibile.
`;

  it("produce chunk non vuoti", () => {
    const chunks = chunker.chunk({
      content: MARKDOWN,
      documentId: "doc-1",
      tenantId: "tenant-a",
      filename: "architecture.md",
    });
    expect(chunks.length).toBeGreaterThan(0);
  });

  it("ogni chunk ha metadati corretti", () => {
    const chunks = chunker.chunk({
      content: MARKDOWN,
      documentId: "doc-1",
      tenantId: "tenant-a",
      filename: "architecture.md",
    });
    for (const chunk of chunks) {
      expect(chunk.id).toBeTruthy();
      expect(chunk.content.trim().length).toBeGreaterThan(0);
      expect(chunk.metadata.document_id).toBe("doc-1");
      expect(chunk.metadata.tenant_id).toBe("tenant-a");
      expect(chunk.metadata.source_type).toBe("markdown");
    }
  });

  it("total_chunks coerente", () => {
    const chunks = chunker.chunk({
      content: MARKDOWN,
      documentId: "doc-2",
      tenantId: "tenant-a",
      filename: "doc.md",
    });
    const total = chunks[0]?.metadata.total_chunks ?? 0;
    expect(total).toBeGreaterThan(0);
  });

  it("nessun chunk supera maxChunkSize", () => {
    const chunks = chunker.chunk({
      content: MARKDOWN,
      documentId: "doc-3",
      tenantId: "tenant-a",
      filename: "doc.md",
    });
    for (const chunk of chunks) {
      expect(chunk.content.length).toBeLessThanOrEqual(600);
    }
  });
});

describe("Chunker — codice TypeScript", () => {
  const chunker = new Chunker({ maxChunkSize: 800, minChunkSize: 30 });

  const CODE = `
export interface LLMRequest {
  model: string;
  messages: LLMMessage[];
}

export async function complete(req: LLMRequest): Promise<LLMResponse> {
  const provider = getProvider(req.model);
  return provider.complete(req);
}

export class LLMGateway {
  private providers = new Map<string, LLMProvider>();

  register(provider: LLMProvider) {
    this.providers.set(provider.name, provider);
  }
}
`;

  it("rileva correttamente source_type code", () => {
    const chunks = chunker.chunk({
      content: CODE,
      documentId: "doc-ts",
      tenantId: "tenant-a",
      filename: "gateway.ts",
    });
    expect(chunks.length).toBeGreaterThan(0);
    expect(chunks[0].metadata.source_type).toBe("code");
    expect(chunks[0].metadata.language).toBe("typescript");
  });

  it("produce chunk con content sensato", () => {
    const chunks = chunker.chunk({
      content: CODE,
      documentId: "doc-ts",
      tenantId: "tenant-a",
      filename: "gateway.ts",
    });
    const allContent = chunks.map((c) => c.content).join("\n");
    expect(allContent).toContain("LLMGateway");
  });
});

describe("Chunker — testo libero", () => {
  const chunker = new Chunker({ maxChunkSize: 200, minChunkSize: 20 });

  it("divide per paragrafo", () => {
    const text = "Paragrafo uno.\n\nParagrafo due.\n\nParagrafo tre.";
    const chunks = chunker.chunk({
      content: text,
      documentId: "doc-txt",
      tenantId: "t1",
    });
    expect(chunks.length).toBeGreaterThanOrEqual(1);
    expect(chunks[0].metadata.source_type).toBe("text");
  });
});

describe("Chunker — sensitivity tier", () => {
  it("propaga il tier ai metadati del chunk", () => {
    const chunker = new Chunker({ minChunkSize: 10 });
    const content = "Contenuto di test con dati riservati classificati come tier 2.";
    const chunks = chunker.chunk({
      content,
      documentId: "doc-sec",
      tenantId: "t1",
      sensitivityTier: 2,
    });
    expect(chunks.length).toBeGreaterThan(0);
    expect(chunks[0].metadata.sensitivity_tier).toBe(2);
  });
});
