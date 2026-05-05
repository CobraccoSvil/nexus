/**
 * Proxy per le impostazioni admin.
 * GET /api/admin/settings → mcp-core GET /api/admin/settings
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

export async function GET(request: NextRequest): Promise<NextResponse> {
  try {
    const response = await fetch(`${CORE_URL}/api/admin/settings`, {
      method: "GET",
      headers: forwardHeaders(request),
    });

    if (!response.ok) {
      return NextResponse.json(
        { error: `Errore mcp-core: ${response.status} ${response.statusText}` },
        { status: response.status },
      );
    }

    const data = await response.json();
    return NextResponse.json(data);
  } catch (err) {
    console.error("[api/admin/settings] Errore connessione mcp-core:", err);
    return NextResponse.json(
      { error: "Impossibile connettersi al servizio core" },
      { status: 502 },
    );
  }
}
