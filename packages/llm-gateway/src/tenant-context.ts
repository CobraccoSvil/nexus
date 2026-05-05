import type { Sql, TransactionSql } from "postgres";
import { AuthError } from "@nexus/shared";

// Propaga il tenant_id corrente via SET LOCAL in ogni transazione Postgres.
// RLS usa current_setting('app.current_tenant_id', true) come predicato.
// SET LOCAL garantisce che il valore sia isolato alla transazione corrente —
// nessun rischio di cross-tenant leakage tra connessioni nel pool.
export class TenantContext {
  constructor(private sql: Sql) {}

  async withTenant<T>(tenantId: string, fn: (sql: TransactionSql) => Promise<T>): Promise<T> {
    if (!tenantId || tenantId.trim() === "") {
      throw new AuthError("tenant_id mancante — impossibile impostare contesto RLS");
    }

    return this.sql.begin(async (txSql: TransactionSql) => {
      await txSql`SELECT set_config('app.current_tenant_id', ${tenantId}, true)`;
      return fn(txSql);
    }) as unknown as Promise<T>;
  }
}

// Utility: estrae tenant_id da un header HTTP Authorization Bearer JWT già validato.
// Il payload deve essere stato pre-validato da JWTService.verify().
export function extractTenantId(
  jwtPayload: { tid?: string } | null | undefined
): string {
  if (!jwtPayload?.tid) {
    throw new AuthError("JWT non contiene il claim tid (tenant_id)");
  }
  return jwtPayload.tid;
}
