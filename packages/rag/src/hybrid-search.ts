import type { Chunk } from "@nexus/embeddings";
import type { Sql } from "postgres";

export interface SearchFilters {
  tenant_id: string;
  project_id?: string;
  sensitivity_tier_max?: number;
  source_type?: string;
  language?: string;
}

export interface VectorResult {
  chunk: Chunk;
  vector_score: number;
  bm25_score: number;
}

// Pesi per la fusione Reciprocal Rank Fusion
const RRF_K = 60;

export class HybridSearch {
  constructor(private sql: Sql) {}

  // Vector similarity search via pgvector
  async vectorSearch(
    queryEmbedding: number[],
    filters: SearchFilters,
    topK = 20
  ): Promise<VectorResult[]> {
    const vectorLiteral = `[${queryEmbedding.join(",")}]`;

    const rows = await this.sql<{
      id: string;
      content: string;
      metadata: Record<string, unknown>;
      sensitivity_tier: number;
      distance: number;
    }[]>`
      SELECT
        id,
        content,
        metadata,
        sensitivity_tier,
        1 - (embedding <=> ${vectorLiteral}::vector) AS distance
      FROM embeddings
      WHERE
        tenant_id = ${filters.tenant_id}
        ${filters.sensitivity_tier_max != null
          ? this.sql`AND sensitivity_tier <= ${filters.sensitivity_tier_max}`
          : this.sql``}
        ${filters.source_type
          ? this.sql`AND metadata->>'source_type' = ${filters.source_type}`
          : this.sql``}
        ${filters.language
          ? this.sql`AND metadata->>'language' = ${filters.language}`
          : this.sql``}
      ORDER BY embedding <=> ${vectorLiteral}::vector
      LIMIT ${topK}
    `;

    return rows.map((row) => ({
      chunk: {
        id: row.id,
        content: row.content,
        metadata: row.metadata as unknown as Chunk["metadata"],
      },
      vector_score: row.distance,
      bm25_score: 0,
    }));
  }

  // BM25-like full-text search via pg_trgm
  async bm25Search(
    query: string,
    filters: SearchFilters,
    topK = 20
  ): Promise<VectorResult[]> {
    const rows = await this.sql<{
      id: string;
      content: string;
      metadata: Record<string, unknown>;
      sensitivity_tier: number;
      similarity: number;
    }[]>`
      SELECT
        id,
        content,
        metadata,
        sensitivity_tier,
        similarity(content, ${query}) AS similarity
      FROM embeddings
      WHERE
        tenant_id = ${filters.tenant_id}
        AND content % ${query}
        ${filters.sensitivity_tier_max != null
          ? this.sql`AND sensitivity_tier <= ${filters.sensitivity_tier_max}`
          : this.sql``}
      ORDER BY similarity DESC
      LIMIT ${topK}
    `;

    return rows.map((row) => ({
      chunk: {
        id: row.id,
        content: row.content,
        metadata: row.metadata as unknown as Chunk["metadata"],
      },
      vector_score: 0,
      bm25_score: row.similarity,
    }));
  }

  // Reciprocal Rank Fusion — fonde i due ranking in uno solo
  fuseRRF(
    vectorResults: VectorResult[],
    bm25Results: VectorResult[]
  ): VectorResult[] {
    const scores = new Map<string, { result: VectorResult; score: number }>();

    for (const [rank, r] of vectorResults.entries()) {
      const rrf = 1 / (RRF_K + rank + 1);
      const existing = scores.get(r.chunk.id);
      if (existing) {
        existing.score += rrf;
      } else {
        scores.set(r.chunk.id, {
          result: { ...r },
          score: rrf,
        });
      }
    }

    for (const [rank, r] of bm25Results.entries()) {
      const rrf = 1 / (RRF_K + rank + 1);
      const existing = scores.get(r.chunk.id);
      if (existing) {
        existing.score += rrf;
        existing.result.bm25_score = r.bm25_score;
      } else {
        scores.set(r.chunk.id, {
          result: { ...r },
          score: rrf,
        });
      }
    }

    return [...scores.values()]
      .sort((a, b) => b.score - a.score)
      .map(({ result, score }) => ({
        ...result,
        vector_score: score,
      }));
  }
}
