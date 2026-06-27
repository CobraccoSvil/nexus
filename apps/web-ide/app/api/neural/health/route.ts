import { NextResponse } from "next/server";

// Il brain Python e' stato eliminato: gli endpoint neural sono ri-esposti in
// mcp-core (porta 4000) sotto /api/neural/*. Stessa convenzione BACKEND_URL del
// fallback /api/:path* (next.config.ts) — niente env var dedicata al brain.
const CORE_URL = process.env.BACKEND_URL || "http://localhost:4000";

export async function GET() {
  try {
    const targetUrl = `${CORE_URL}/api/neural/health`;
    console.log(`[API] Richiesta neural a mcp-core: ${targetUrl}`);

    const response = await fetch(targetUrl, {
      method: "GET",
      headers: { "Content-Type": "application/json" },
    });

    const data = await response.json();
    console.log(`[API] Risposta neural da mcp-core: status=${response.status}`);

    return NextResponse.json(data, { status: response.status });
  } catch (error: unknown) {
    const errorMsg = error instanceof Error ? error.message : String(error);
    console.error("Neural (mcp-core) health endpoint error:", errorMsg);
    return NextResponse.json(
      { error: `Neural (mcp-core) non disponibile: ${errorMsg}` },
      { status: 503 }
    );
  }
}
