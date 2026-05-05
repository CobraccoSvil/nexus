import { describe, it, expect } from "vitest";
import { Reranker } from "../src/reranker.js";
import type { Chunk } from "../src/types.js";

const makeChunk = (content: string, id: string): Chunk => ({
  id,
  content,
  metadata: {
    document_id: "doc-1",
    tenant_id: "tenant-a",
    chunk_index: 0,
    total_chunks: 1,
    source_type: "text",
  },
});

describe("Reranker — rerankSync (keyword overlap, senza modello)", () => {
  const reranker = new Reranker();

  it("ordina per rilevanza con la query", () => {
    const chunks = [
      makeChunk("Il database PostgreSQL supporta pgvector per embedding.", "c1"),
      makeChunk("Il framework React è usato per UI.", "c2"),
      makeChunk("pgvector permette similarity search su PostgreSQL.", "c3"),
    ];

    const ranked = reranker.rerankSync("pgvector PostgreSQL", chunks);

    expect(ranked[0].id).not.toBe("c2");
    expect(ranked[0].rerank_score).toBeGreaterThan(0);
  });

  it("topK limita i risultati", () => {
    const chunks = [
      makeChunk("Chunk uno", "c1"),
      makeChunk("Chunk due", "c2"),
      makeChunk("Chunk tre", "c3"),
    ];
    const ranked = reranker.rerankSync("uno due tre", chunks, 2);
    expect(ranked.length).toBe(2);
  });

  it("lista vuota → lista vuota", () => {
    const ranked = reranker.rerankSync("query", []);
    expect(ranked).toEqual([]);
  });

  it("ogni chunk ha rerank_score assegnato", () => {
    const chunks = [
      makeChunk("Vector similarity search", "c1"),
      makeChunk("Retrieval augmented generation", "c2"),
    ];
    const ranked = reranker.rerankSync("vector retrieval", chunks);
    for (const r of ranked) {
      expect(typeof r.rerank_score).toBe("number");
      expect(r.rerank_score).toBeGreaterThanOrEqual(0);
    }
  });
});
