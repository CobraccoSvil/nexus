/**
 * GET /api/system/services
 *
 * Proxy sottile verso mcp-core: lo stato dei microservizi infrastruttura Nexus
 * (Core, Gateway, Admin/Doc/Billing/Plugin, Postgres, Redis) e' calcolato in
 * modo PLATFORM-AWARE dal backend (crate system_services), non piu' via
 * `systemctl` + `child_process` qui — che su Windows nativo fallivano sempre e
 * mascheravano lo stato "unknown" ad "active" con un'euristica port_alive.
 *
 * Fonte di verita': il catalogo DB `system.services_catalog` (migrazione 0541).
 * Lo stato e' un TCP probe onesto della porta risolta dal DB (regola G/M).
 *
 * Nota: essendo un proxy, quando mcp-core e' offline questo endpoint ritorna 503
 * e il pannello mantiene l'ultimo stato noto (limite accettato dell'architettura
 * "endpoint mcp-core + proxy": il recovery di un Core caduto avviene con
 * deploy/dev-start.ps1).
 */
import { NextResponse } from "next/server";

const MCP_CORE_URL =
  process.env.MCP_CORE_URL || process.env.CORE_SERVICE_URL || "http://localhost:4000";

export async function GET(request: Request) {
  const targetUrl = `${MCP_CORE_URL}/api/system/services`;
  try {
    const cookieHeader = request.headers.get("cookie") ?? "";
    const response = await fetch(targetUrl, {
      headers: { ...(cookieHeader ? { Cookie: cookieHeader } : {}) },
      cache: "no-store",
    });
    const data = await response.json();
    return NextResponse.json(data, { status: response.status });
  } catch (error) {
    console.error("[API Proxy] GET system/services error:", error);
    // mcp-core non raggiungibile: il pannello ignora il 503 e tiene l'ultimo stato.
    return NextResponse.json({ services: [] }, { status: 503 });
  }
}
