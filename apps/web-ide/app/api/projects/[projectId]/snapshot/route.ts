/**
 * Proxy per lo snapshot REST del dispatcher di mcp-core.
 *
 * Complementare alla route event-stream: fornisce lo stato iniziale
 * del progetto prima di aprire la connessione SSE.
 * Propaga esplicitamente il cookie di autenticazione.
 */

import { NextResponse } from "next/server";

const CORE_URL =
  process.env.CORE_SERVICE_URL ||
  process.env.MCP_CORE_URL ||
  process.env.BACKEND_URL ||
  "http://127.0.0.1:4000";

type Params = { projectId: string };

export async function GET(
  request: Request,
  { params }: { params: Promise<Params> },
) {
  const { projectId } = await params;
  const { searchParams } = new URL(request.url);
  const qs = searchParams.toString();
  const backendUrl = `${CORE_URL}/api/projects/${projectId}/snapshot${qs ? `?${qs}` : ""}`;

  const headers: Record<string, string> = {};
  const cookie = request.headers.get("cookie");
  if (cookie) headers["cookie"] = cookie;

  try {
    const response = await fetch(backendUrl, { headers, cache: "no-store" });
    if (!response.ok) {
      const body = await response.text().catch(() => "");
      return new Response(body, {
        status: response.status,
        headers: { "Content-Type": response.headers.get("content-type") || "application/json" },
      });
    }
    const data = await response.json();
    return NextResponse.json(data);
  } catch (err) {
    console.error(`[snapshot proxy] projects/${projectId}/snapshot:`, err);
    return NextResponse.json(
      { error: "Backend non raggiungibile" },
      { status: 502 },
    );
  }
}
