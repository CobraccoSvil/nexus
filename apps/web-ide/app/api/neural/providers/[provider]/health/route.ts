import { NextResponse } from "next/server";

// Il brain Python e' stato eliminato: gli endpoint neural sono ri-esposti in
// mcp-core (porta 4000) sotto /api/neural/*. Stessa convenzione BACKEND_URL del
// fallback /api/:path* (next.config.ts) — niente env var dedicata al brain.
const CORE_URL = process.env.BACKEND_URL || "http://localhost:4000";

export async function GET(
  request: Request,
  { params }: { params: Promise<{ provider: string }> }
) {
  const { provider } = await params;

  try {
    const response = await fetch(`${CORE_URL}/api/neural/providers/${provider}/health`, {
      method: "GET",
      headers: { "Content-Type": "application/json" },
    });

    const data = await response.json();
    return NextResponse.json(data, { status: response.status });
  } catch (error) {
    console.error(`Provider ${provider} health endpoint error:`, error);
    return NextResponse.json(
      { error: `Provider ${provider} non disponibile` },
      { status: 503 }
    );
  }
}
