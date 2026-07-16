/**
 * Proxy per il salvataggio di una singola impostazione admin.
 * PUT /api/admin/setting/:key → mcp-core PUT /api/admin/setting/:key
 * Passa cookie e Authorization header per autenticazione admin.
 */

import { NextRequest, NextResponse } from "next/server";

const CORE_URL = process.env.CORE_SERVICE_URL || "http://127.0.0.1:4000";

function forwardHeaders(request: NextRequest): HeadersInit {
  const headers: Record<string, string> = { "Content-Type": "application/json" };
  const cookie = request.headers.get("cookie");
  if (cookie) headers["cookie"] = cookie;
  const auth = request.headers.get("authorization");
  if (auth) headers["authorization"] = auth;
  return headers;
}

export async function PUT(
  request: NextRequest,
  { params }: { params: Promise<{ key: string }> },
): Promise<NextResponse> {
  try {
    const { key } = await params;
    const body = await request.text();

    const response = await fetch(`${CORE_URL}/api/admin/setting/${key}`, {
      method: "PUT",
      headers: forwardHeaders(request),
      body,
    });

    const data = await response.json().catch(() => null);

    // Lo status di mcp-core passa intatto (200 aggiornata, 404 chiave assente,
    // 500 rifiutata dal DB), e con esso il body: contiene il motivo del rifiuto
    // — es. il guard sui setting protetti della mig 0499. Sostituirlo con lo
    // status testuale lascerebbe l'admin senza la ragione dell'errore.
    if (data === null) {
      return NextResponse.json(
        { error: `Errore mcp-core: ${response.status} ${response.statusText}` },
        { status: response.ok ? 502 : response.status },
      );
    }
    return NextResponse.json(data, { status: response.status });
  } catch (err) {
    console.error("[api/admin/setting/[key]] Errore connessione mcp-core:", err);
    return NextResponse.json(
      { error: "Impossibile connettersi al servizio core" },
      { status: 502 },
    );
  }
}
