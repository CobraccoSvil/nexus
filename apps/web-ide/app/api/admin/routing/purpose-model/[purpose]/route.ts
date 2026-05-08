/**
 * Proxy per l'aggiornamento di un singolo purpose model.
 * PUT /api/admin/routing/purpose-model/:purpose → mcp-core PUT /api/admin/routing/purpose-model/:purpose
 * Passa cookie e Authorization header per autenticazione admin.
 */

import { NextRequest, NextResponse } from "next/server";

const CORE_URL = process.env.CORE_SERVICE_URL || "http://127.0.0.1:4000";

function forwardHeaders(request: NextRequest): HeadersInit {
  const headers: HeadersInit = { "Content-Type": "application/json" };
  const cookie = request.headers.get("cookie");
  if (cookie) headers["cookie"] = cookie;
  const auth = request.headers.get("authorization");
  if (auth) headers["authorization"] = auth;
  return headers;
}

export async function PUT(
  request: NextRequest,
  { params }: { params: Promise<{ purpose: string }> },
): Promise<NextResponse> {
  try {
    const { purpose } = await params;
    const body = await request.json();

    const response = await fetch(
      `${CORE_URL}/api/admin/routing/purpose-model/${encodeURIComponent(purpose)}`,
      {
        method: "PUT",
        headers: forwardHeaders(request),
        body: JSON.stringify(body),
      },
    );

    if (!response.ok) {
      return NextResponse.json(
        { error: `Errore mcp-core: ${response.status} ${response.statusText}` },
        { status: response.status },
      );
    }

    const data = await response.json();
    return NextResponse.json(data);
  } catch (err) {
    console.error("[api/admin/routing/purpose-model] Errore connessione mcp-core:", err);
    return NextResponse.json(
      { error: "Impossibile connettersi al servizio core" },
      { status: 502 },
    );
  }
}
