import { NextResponse } from "next/server";

// Il brain Python e' stato eliminato: gli endpoint neural sono ri-esposti in
// mcp-core (porta 4000) sotto /api/neural/*. Stessa convenzione BACKEND_URL del
// fallback /api/:path* (next.config.ts) — niente env var dedicata al brain.
const CORE_URL = process.env.BACKEND_URL || "http://localhost:4000";

export async function GET(
  request: Request,
  { params }: { params: Promise<{ paths?: string[] }> }
) {
  const { paths = [] } = await params;
  if (!paths || paths.length === 0) {
    return NextResponse.json({ error: "Path non specificato" }, { status: 400 });
  }

  const pathStr = paths.join("/");
  const targetUrl = `${CORE_URL}/api/neural/providers/${pathStr}`;

  try {
    console.log(`[API Proxy] GET ${targetUrl}`);
    const response = await fetch(targetUrl, {
      method: "GET",
      headers: { "Content-Type": "application/json" },
    });

    const data = await response.json();
    return NextResponse.json(data, { status: response.status });
  } catch (error: unknown) {
    const msg = error instanceof Error ? error.message : String(error);
    console.error(`Neural GET /providers/${pathStr} error:`, msg);
    return NextResponse.json(
      { error: "Provider non disponibile" },
      { status: 503 }
    );
  }
}

export async function POST(
  request: Request,
  { params }: { params: Promise<{ paths?: string[] }> }
) {
  const { paths = [] } = await params;
  const pathStr = paths.join("/");
  const targetUrl = `${CORE_URL}/api/neural/providers/${pathStr}`;

  try {
    const body = await request.json().catch(() => null);
    const response = await fetch(targetUrl, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: body ? JSON.stringify(body) : undefined,
    });

    const data = await response.json();
    return NextResponse.json(data, { status: response.status });
  } catch (error) {
    console.error(`Neural POST /providers/${pathStr} error:`, error);
    return NextResponse.json(
      { error: "Provider non disponibile" },
      { status: 503 }
    );
  }
}
