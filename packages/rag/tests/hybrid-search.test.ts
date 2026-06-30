import { describe, it, expect, vi } from "vitest";
import { HybridSearch } from "../src/hybrid-search.js";
import type { Chunk } from "@nexus/embeddings";

function makeResult(id: string, content: string, vectorScore: number) {
  return {
    chunk: { id, content, metadata: { document_id: "d1", tenant_id: "t1", chunk_index: 0, total_chunks: 1, source_type: "text" as const } },
    vector_score: vectorScore,
    bm25_score: 0,
  };
}

describe("HybridSearch — fuseRRF", () => {
  const sql = {} as any;
  const search = new HybridSearch(sql);

  it("risultati di entrambe le sorgenti vengono fusi", () => {
    const vector = [makeResult("c1", "text1", 0.9), makeResult("c2", "text2", 0.8)];
    const bm25 = [makeResult("c2", "text2", 0.7), makeResult("c3", "text3", 0.6)];

    const fused = search.fuseRRF(vector, bm25);

    expect(fused.map((r) => r.chunk.id)).toContain("c1");
    expect(fused.map((r) => r.chunk.id)).toContain("c2");
    expect(fused.map((r) => r.chunk.id)).toContain("c3");
  });

  it("chunk in entrambe le sorgenti ha score più alto", () => {
    const vector = [makeResult("shared", "overlap", 0.9), makeResult("only-v", "vect", 0.8)];
    const bm25 = [makeResult("shared", "overlap", 0.7), makeResult("only-b", "bm25", 0.6)];

    const fused = search.fuseRRF(vector, bm25);
    const sharedScore = fused.find((r) => r.chunk.id === "shared")?.vector_score ?? 0;
    const onlyVScore = fused.find((r) => r.chunk.id === "only-v")?.vector_score ?? 0;
    const onlyBScore = fused.find((r) => r.chunk.id === "only-b")?.vector_score ?? 0;

    // shared è in entrambi → score più alto di quelli solo in una
    expect(sharedScore).toBeGreaterThan(onlyVScore);
    expect(sharedScore).toBeGreaterThan(onlyBScore);
  });

  it("lista vuota → lista vuota", () => {
    expect(search.fuseRRF([], [])).toEqual([]);
  });

  it("ordine decrescente per score", () => {
    const v = [makeResult("a", "a", 0.9), makeResult("b", "b", 0.5)];
    const fused = search.fuseRRF(v, []);
    const scores = fused.map((r) => r.vector_score);
    for (let i = 1; i < scores.length; i++) {
      expect(scores[i - 1]).toBeGreaterThanOrEqual(scores[i]);
    }
  });

  it("senza duplicati nell'output", () => {
    const v = [makeResult("dup", "dup", 0.9)];
    const b = [makeResult("dup", "dup", 0.8)];
    const fused = search.fuseRRF(v, b);
    const ids = fused.map((r) => r.chunk.id);
    expect(new Set(ids).size).toBe(ids.length);
  });
});
