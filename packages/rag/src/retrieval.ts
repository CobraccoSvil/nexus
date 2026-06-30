import type { Chunk, RankedChunk } from "@nexus/embeddings";
import { OnnxEmbeddingRunner, Reranker } from "@nexus/embeddings";
import type { Sql } from "postgres";
import { HybridSearch, type SearchFilters } from "./hybrid-search.js";

export interface RetrievalOptions {
  topK?: number;           // chunk da restituire dopo reranking (default 5)
  candidateK?: number;     // candidati da recuperare prima del reranking (default 20)
  useReranker?: boolean;   // abilita reranker (default true)
  embeddingCache?: Map<string, number[]>;   // cache query embedding (estesa: Redis in prod)
}

const DEFAULT_OPTS: Required<RetrievalOptions> = {
  topK: 5,
  candidateK: 20,
  useReranker: true,
  embeddingCache: new Map(),
};

export class RetrievalPipeline {
  private embedder: OnnxEmbeddingRunner;
  private reranker: Reranker;
  private search: HybridSearch;

  constructor(sql: Sql) {
    this.embedder = new OnnxEmbeddingRunner();
    this.reranker = new Reranker();
    this.search = new HybridSearch(sql);
  }

  async retrieve(
    query: string,
    filters: SearchFilters,
    opts: RetrievalOptions = {}
  ): Promise<RankedChunk[]> {
    const { topK, candidateK, useReranker, embeddingCache } = {
      ...DEFAULT_OPTS,
      ...opts,
    };

    // 1. Embedding della query con cache
    let queryEmbedding = embeddingCache.get(query);
    if (!queryEmbedding) {
      const vec = await this.embedder.embedSingle(query);
      queryEmbedding = Array.from(vec.values);
      embeddingCache.set(query, queryEmbedding);
    }

    // 2. Retrieval ibrido parallelo (vector + BM25)
    const [vectorResults, bm25Results] = await Promise.all([
      this.search.vectorSearch(queryEmbedding, filters, candidateK),
      this.search.bm25Search(query, filters, candidateK),
    ]);

    // 3. Fusione RRF
    const fused = this.search.fuseRRF(vectorResults, bm25Results);
    const candidates: Chunk[] = fused.slice(0, candidateK).map((r) => r.chunk);

    if (candidates.length === 0) return [];

    // 4. Reranking
    if (useReranker) {
      return this.reranker.rerank(query, candidates, topK);
    }

    // Fallback: ritorna candidati con score RRF come score
    return candidates.slice(0, topK).map((chunk, i) => ({
      ...chunk,
      score: fused[i]?.vector_score ?? 0,
    }));
  }
}
