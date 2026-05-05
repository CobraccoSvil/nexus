import type { Chunk } from "@nexus/embeddings";
import type { Sql } from "postgres";

export interface IngestionResult {
  document_id: string;
  chunks_processed: number;
  chunks_upserted: number;
  duration_ms: number;
}

export class IngestionPipeline {
  constructor(private sql: Sql) {}

  async upsertChunks(
    chunks: Chunk[],
    embeddings: number[][]
  ): Promise<IngestionResult> {
    if (chunks.length === 0 || chunks.length !== embeddings.length) {
      throw new Error("chunks e embeddings devono avere la stessa lunghezza non-zero");
    }

    const start = Date.now();
    const documentId = chunks[0].metadata.document_id;
    let upserted = 0;

    // Upsert in batch da 100 per evitare timeout su ingest grandi
    const BATCH = 100;
    for (let i = 0; i < chunks.length; i += BATCH) {
      const batch = chunks.slice(i, i + BATCH);
      const embBatch = embeddings.slice(i, i + BATCH);

      await this.sql.begin(async (tx) => {
        for (let j = 0; j < batch.length; j++) {
          const chunk = batch[j];
          const embedding = embBatch[j];
          const vectorLiteral = `[${embedding.join(",")}]`;

          await tx`
            INSERT INTO embeddings (
              id, tenant_id, document_id, chunk_index, content,
              embedding, metadata, sensitivity_tier
            ) VALUES (
              ${chunk.id},
              ${chunk.metadata.tenant_id},
              ${chunk.metadata.document_id},
              ${chunk.metadata.chunk_index},
              ${chunk.content},
              ${vectorLiteral}::vector,
              ${JSON.stringify(chunk.metadata)},
              ${chunk.metadata.sensitivity_tier ?? 0}
            )
            ON CONFLICT (id) DO UPDATE SET
              content = EXCLUDED.content,
              embedding = EXCLUDED.embedding,
              metadata = EXCLUDED.metadata,
              updated_at = NOW()
          `;
          upserted++;
        }
      });
    }

    return {
      document_id: documentId,
      chunks_processed: chunks.length,
      chunks_upserted: upserted,
      duration_ms: Date.now() - start,
    };
  }

  async deleteDocument(documentId: string, tenantId: string): Promise<number> {
    const result = await this.sql`
      DELETE FROM embeddings
      WHERE document_id = ${documentId} AND tenant_id = ${tenantId}
      RETURNING id
    `;
    return result.length;
  }

  async documentExists(documentId: string, tenantId: string): Promise<boolean> {
    const rows = await this.sql`
      SELECT 1 FROM embeddings
      WHERE document_id = ${documentId} AND tenant_id = ${tenantId}
      LIMIT 1
    `;
    return rows.length > 0;
  }
}
