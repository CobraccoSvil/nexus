import type { RankedChunk, Chunk } from "./types.js";

const RERANKER_MODEL = "Xenova/bge-reranker-v2-m3";

let rerankerPipeline: Awaited<ReturnType<typeof import("@xenova/transformers").pipeline>> | null = null;

async function getReranker() {
  if (rerankerPipeline) return rerankerPipeline;
  const { pipeline, env } = await import("@xenova/transformers");
  env.allowRemoteModels = process.env.NODE_ENV !== "production";
  env.localModelPath = process.env.MODEL_CACHE_DIR ?? "./models";
  rerankerPipeline = await pipeline("text-classification", RERANKER_MODEL, {
    quantized: true,
  });
  return rerankerPipeline;
}

export class Reranker {
  readonly model = RERANKER_MODEL;

  async rerank(query: string, chunks: Chunk[], topK?: number): Promise<RankedChunk[]> {
    if (chunks.length === 0) return [];

    const reranker = await getReranker();

    // bge-reranker-v2-m3: input è array di {text, text_pair}, output è score di rilevanza
    const inputs = chunks.map((c) => ({ text: query, text_pair: c.content }));

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const scores: { score: number }[] = await (reranker as any)(inputs, {
      function_to_apply: "sigmoid",
    });

    const ranked: RankedChunk[] = chunks.map((chunk, i) => ({
      ...chunk,
      score: scores[i]?.score ?? 0,
      rerank_score: scores[i]?.score ?? 0,
    }));

    ranked.sort((a, b) => (b.rerank_score ?? 0) - (a.rerank_score ?? 0));

    return topK ? ranked.slice(0, topK) : ranked;
  }

  // Versione senza modello: score basato su keyword overlap (fallback per dev)
  rerankSync(query: string, chunks: Chunk[], topK?: number): RankedChunk[] {
    const queryTerms = query.toLowerCase().split(/\s+/);

    const ranked: RankedChunk[] = chunks.map((chunk) => {
      const text = chunk.content.toLowerCase();
      const overlap = queryTerms.filter((t) => text.includes(t)).length;
      const score = overlap / queryTerms.length;
      return { ...chunk, score, rerank_score: score };
    });

    ranked.sort((a, b) => (b.rerank_score ?? 0) - (a.rerank_score ?? 0));
    return topK ? ranked.slice(0, topK) : ranked;
  }
}
