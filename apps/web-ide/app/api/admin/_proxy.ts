/**
 * Helper condiviso per le route proxy admin verso mcp-core.
 * Propaga automaticamente cookie e Authorization dalla richiesta browser.
 */

import { NextRequest, NextResponse } from "next/server";

export const CORE_URL = process.env.CORE_SERVICE_URL || "http://127.0.0.1:4000";

export function forwardHeaders(request: NextRequest): Record<string, string> {
  const headers: Record<string, string> = { "Content-Type": "application/json" };
  const cookie = request.headers.get("cookie");
  if (cookie) headers["cookie"] = cookie;
  const auth = request.headers.get("authorization");
  if (auth) headers["authorization"] = auth;
  return headers;
}

export async function proxyRequest(
  request: NextRequest,
  backendPath: string,
  method?: string,
): Promise<NextResponse> {
  try {
    const { searchParams } = new URL(request.url);
    const qs = searchParams.toString();
    const url = `${CORE_URL}${backendPath}${qs ? `?${qs}` : ""}`;
    const m = method ?? request.method;

    const hasBody = !["GET", "HEAD", "DELETE"].includes(m.toUpperCase());
    const body = hasBody ? await request.text() : undefined;

    const response = await fetch(url, {
      method: m,
      headers: forwardHeaders(request),
      body,
    });

    if (!response.ok) {
      return NextResponse.json(
        { error: `Errore mcp-core: ${response.status} ${response.statusText}` },
        { status: response.status },
      );
    }

    const contentType = response.headers.get("content-type") ?? "";
    if (contentType.includes("application/json")) {
      const data = await response.json();
      return NextResponse.json(data);
    }
    const text = await response.text();
    return new NextResponse(text, { status: response.status, headers: { "Content-Type": contentType } });
  } catch (err) {
    console.error(`[proxy] Errore ${backendPath}:`, err);
    return NextResponse.json({ error: "Impossibile connettersi al servizio core" }, { status: 502 });
  }
}
