import { NextRequest, NextResponse } from "next/server";

const CORE_URL = process.env.CORE_SERVICE_URL || "http://127.0.0.1:4000";

/** GET /api/models — proxy verso mcp-core /api/models (richiede auth). */
export async function GET(request: NextRequest) {
  const cookie = request.headers.get("cookie") ?? "";
  const auth = request.headers.get("authorization") ?? "";

  const headers: Record<string, string> = { "Content-Type": "application/json" };
  if (cookie) headers["cookie"] = cookie;
  if (auth) headers["authorization"] = auth;

  try {
    const response = await fetch(`${CORE_URL}/api/models`, { method: "GET", headers });
    const bodyText = await response.text();

    if (!response.ok) {
      return NextResponse.json(
        { error: `Errore mcp-core: ${response.status} ${response.statusText}` },
        { status: response.status },
      );
    }

    try {
      const data = JSON.parse(bodyText);
      return NextResponse.json(data);
    } catch {
      return NextResponse.json(
        { error: "Risposta non-JSON da mcp-core" },
        { status: 502 },
      );
    }
  } catch {
    return NextResponse.json(
      { error: "Impossibile connettersi al servizio core" },
      { status: 502 },
    );
  }
}
