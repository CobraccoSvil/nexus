import type { Sql } from "postgres";
import type { AuditRecord } from "./logger.js";

// Scrive i record audit nel DB con retry semplice.
// Il fallimento non deve mai bloccare la risposta all'utente.
export class AuditWriter {
  constructor(private sql: Sql) {}

  async write(record: AuditRecord): Promise<void> {
    try {
      await this.sql`
        INSERT INTO audit_llm_calls (
          request_id, tenant_id, user_id, feature, sensitivity_tier,
          model_requested, model_used, provider_used,
          prompt_hash, response_hash,
          input_tokens, output_tokens, latency_ms, finish_reason,
          redaction_applied, dlp_blocked, dlp_patterns,
          created_at, retention_until
        ) VALUES (
          ${record.request_id},
          ${record.tenant_id},
          ${record.user_id},
          ${record.feature},
          ${record.sensitivity_tier},
          ${record.model_requested},
          ${record.model_used},
          ${record.provider_used},
          ${record.prompt_hash},
          ${record.response_hash},
          ${record.input_tokens},
          ${record.output_tokens},
          ${record.latency_ms},
          ${record.finish_reason},
          ${record.redaction_applied},
          ${record.dlp_blocked},
          ${record.dlp_patterns},
          ${record.timestamp}::timestamptz,
          ${record.retention_until}::timestamptz
        )
        ON CONFLICT (request_id) DO NOTHING
      `;
    } catch (err) {
      // Log del fallimento ma non rilancia — l'audit non deve bloccare la response
      console.error("[audit-writer] failed to write record", {
        request_id: record.request_id,
        error: err instanceof Error ? err.message : String(err),
      });
    }
  }

  async queryByTenant(tenantId: string, limit = 100): Promise<AuditRecord[]> {
    const rows = await this.sql<AuditRecord[]>`
      SELECT *
      FROM audit_llm_calls
      WHERE tenant_id = ${tenantId}
        AND retention_until > NOW()
      ORDER BY created_at DESC
      LIMIT ${limit}
    `;
    return rows;
  }
}
