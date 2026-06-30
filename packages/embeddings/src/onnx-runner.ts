import type { EmbeddingVector } from "./types.js";

// Lazy import di @xenova/transformers — i modelli vengono scaricati e cachati
// in ~/.cache/huggingface/hub la prima volta, poi riusati offline.
// In produzione, i modelli sono pre-bundlati nell'image Docker (non pull a runtime).

let pipelineFactory: typeof import("@xenova/transformers").pipeline | null = null;
let featureExtractor: Awaited<ReturnType<typeof import("@xenova/transformers").pipeline>> | null = null;

const MODEL_ID = "Xenova/bge-m3";

async function getExtractor() {
  if (featureExtractor) return featureExtractor;

  if (!pipelineFactory) {
    const { pipeline, env } = await import("@xenova/transformers");
    // Disabilita telemetria verso HuggingFace in produzione
    env.allowRemoteModels = process.env.NODE_ENV !== "production";
    env.localModelPath = process.env.MODEL_CACHE_DIR ?? "./models";
    pipelineFactory = pipeline;
  }

  featureExtractor = await pipelineFactory("feature-extraction", MODEL_ID, {
    quantized: true,  // usa versione INT8 quantizzata (~300MB vs 1.1GB)
  });

  return featureExtractor;
}

export class OnnxEmbeddingRunner {
  readonly model = MODEL_ID;
  readonly dimensions = 1024;

  async embed(texts: string[]): Promise<EmbeddingVector[]> {
    if (texts.length === 0) return [];

    const extractor = await getExtractor();

    // Batch inference — più efficiente di chiamate singole
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const output = await (extractor as any)(texts, {
      pooling: "cls",      // bge-m3 usa CLS token pooling
      normalize: true,     // normalizza per cosine similarity
    }) as { data: Float32Array };

    return texts.map((_, i) => ({
      values: Array.from(output.data.slice(i * this.dimensions, (i + 1) * this.dimensions)),
      dimensions: this.dimensions,
      model: this.model,
    }));
  }

  async embedSingle(text: string): Promise<EmbeddingVector> {
    const [result] = await this.embed([text]);
    return result;
  }

  // Cosine similarity tra due vettori normalizzati (dot product)
  static cosineSimilarity(a: number[], b: number[]): number {
    return a.reduce((sum, v, i) => sum + v * b[i], 0);
  }
}
