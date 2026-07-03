import { NextRequest, NextResponse } from "next/server";

const CORE_URL = process.env.CORE_SERVICE_URL || "http://127.0.0.1:4000";

/**
 * GET /api/ui-flags — proxy verso mcp-core /api/ui-flags (richiede solo auth,
 * NON admin). Espone una whitelist di flag UI non sensibili dal DB settings
 * (es. chat.activity_stream_enabled), leggibili da qualunque utente autenticato:
 * i flag di rendering della chat devono essere letti anche dagli utenti non
 * admin, altrimenti la feature resterebbe attivabile solo per gli admin (ADR
 * 0037). Fonte di verita' nel DB (regola G).
 */
export async function GET(request: NextRequest) {
  const cookie = request.headers.get("cookie") ?? "";
  const auth = request.headers.get("authorization") ?? "";

  const headers: Record<string, string> = { "Content-Type": "application/json" };
  if (cookie) headers["cookie"] = cookie;
  if (auth) headers["authorization"] = auth;

  try {
    const response = await fetch(`${CORE_URL}/api/ui-flags`, { method: "GET", headers });
    const bodyText = await response.text();

    if (!response.ok) {
      return NextResponse.json(
        { error: `Errore mcp-core: ${response.status} ${response.statusText}` },
        { status: response.status },
      );
    }

    try {
      return NextResponse.json(JSON.parse(bodyText));
    } catch {
      return NextResponse.json({ error: "Risposta non-JSON da mcp-core" }, { status: 502 });
    }
  } catch {
    return NextResponse.json({ error: "Impossibile connettersi al servizio core" }, { status: 502 });
  }
}
