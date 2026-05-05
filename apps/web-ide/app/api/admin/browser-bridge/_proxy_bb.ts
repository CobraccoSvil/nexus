/**
 * Proxy verso admin-service (porta 4010) per le route browser-bridge.
 * Le route /api/admin/browser-bridge/* non passano per mcp-core (4000) perche`
 * mcp-core implementa le route admin direttamente — non fa reverse proxy verso
 * admin-service. Questo helper proxa direttamente su admin-service con
 * propagazione dei cookie/token JWT.
 */

import { NextRequest, NextResponse } from "next/server";

const ADMIN_URL =
  process.env.ADMIN_SERVICE_URL || "http://127.0.0.1:4010";

function forwardHeaders(req: NextRequest): Record<string, string> {
  const h: Record<string, string> = { "Content-Type": "application/json" };
  const cookie = req.headers.get("cookie");
  if (cookie) h["cookie"] = cookie;
  const auth = req.headers.get("authorization");
  if (auth) h["authorization"] = auth;
  return h;
}

export async function proxyToAdmin(
  req: NextRequest,
  path: string,
  method = "GET",
): Promise<NextResponse> {
  try {
    const url = `${ADMIN_URL}${path}`;
    const r = await fetch(url, { method, headers: forwardHeaders(req) });
    const contentType = r.headers.get("content-type") ?? "";
    if (contentType.includes("application/json")) {
      const data = await r.json();
      return NextResponse.json(data, { status: r.status });
    }
    const text = await r.text();
    return new NextResponse(text, {
      status: r.status,
      headers: { "Content-Type": contentType || "text/plain" },
    });
  } catch (err) {
    console.error("[proxy-bb]", path, err);
    return NextResponse.json(
      { error: "browser-bridge daemon non raggiungibile" },
      { status: 502 },
    );
  }
}
