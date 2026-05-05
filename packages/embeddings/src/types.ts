export interface EmbeddingVector {
  values: Float32Array | number[];
  dimensions: number;
  model: string;
}

export interface Chunk {
  id: string;
  content: string;
  metadata: ChunkMetadata;
}

export interface ChunkMetadata {
  document_id: string;
  tenant_id: string;
  chunk_index: number;
  total_chunks: number;
  source_type: "markdown" | "code" | "text" | "docx";
  language?: string;          // per codice: "typescript", "python", ecc.
  heading_path?: string[];    // per markdown: ["# Titolo", "## Sezione"]
  char_start?: number;
  char_end?: number;
  sensitivity_tier?: 0 | 1 | 2 | 3;
}

export interface RankedChunk extends Chunk {
  score: number;
  rerank_score?: number;
}
