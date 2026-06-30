import { NextResponse } from "next/server";

// Il brain Python e' stato eliminato: gli endpoint neural sono ri-esposti in
// mcp-core (porta 4000) sotto /api/neural/*. Stessa convenzione BACKEND_URL del
// fallback /api/:path* (next.config.ts) — niente env var dedicata al brain.
const CORE_URL = process.env.BACKEND_URL || "http://localhost:4000";

export async function POST(request: Request) {
  try {
    const body = await request.json();
    const response = await fetch(`${CORE_URL}/api/neural/route-model`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });

    const data = await response.json();
    return NextResponse.json(data, { status: response.status });
  } catch (error) {
    console.error("Route model error:", error);
    return NextResponse.json(
      { error: "Errore nel routing del modello" },
      { status: 500 }
    );
  }
}
