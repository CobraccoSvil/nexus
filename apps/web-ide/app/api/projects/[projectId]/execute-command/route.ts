/**
 * Proxy per l'esecuzione comandi dalla chat dell'IDE.
 *
 * POST /api/projects/:id/execute-command
 * Body: { "command": "...", "timeout_secs": 60 }
 *
 * Inoltra la richiesta a mcp-core propagando il cookie di autenticazione.
 * Il timeout del proxy (130s) copre il timeout massimo backend (120s) + margine.
 */

import { NextResponse } from "next/server";

const CORE_URL =
  process.env.CORE_SERVICE_URL ||
  process.env.MCP_CORE_URL ||
  process.env.BACKEND_URL ||
  "http://127.0.0.1:4000";

type Params = { projectId: string };

export async function POST(
  request: Request,
  { params }: { params: Promise<Params> },
) {
  const { projectId } = await params;
  const backendUrl = `${CORE_URL}/api/projects/${projectId}/execute-command`;

  const headers: Record<string, string> = {
    "Content-Type": "application/json",
  };
  const cookie = request.headers.get("cookie");
  if (cookie) headers["cookie"] = cookie;

  try {
    const body = await request.text();
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 130_000);

    const response = await fetch(backendUrl, {
      method: "POST",
      headers,
      body,
      signal: controller.signal,
      cache: "no-store",
    });
    clearTimeout(timeout);

    const data = await response.text();
    return new Response(data, {
      status: response.status,
      headers: {
        "Content-Type": response.headers.get("content-type") || "application/json",
      },
    });
  } catch (err) {
    const isAbort = err instanceof Error && err.name === "AbortError";
    console.error(`[execute-command proxy] projects/${projectId}:`, err);
    return NextResponse.json(
      {
        exit_code: -1,
        stdout: "",
        stderr: isAbort
          ? "Timeout: il proxy ha superato il limite di 130 secondi"
          : "Backend non raggiungibile",
        blocked: false,
        duration_ms: 0,
      },
      { status: isAbort ? 504 : 502 },
    );
  }
}
