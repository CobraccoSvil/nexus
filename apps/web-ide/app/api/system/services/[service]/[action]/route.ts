/**
 * POST /api/system/services/[service]/[action]
 *
 * Proxy sottile verso mcp-core: start/stop/restart di un microservizio
 * infrastruttura Nexus. Il controllo e' PLATFORM-AWARE nel backend (crate
 * system_services -> systemctl su Unix, deploy/dev-service.ps1 su Windows), non
 * piu' via `systemctl` + `child_process` qui (rotto su Windows nativo).
 *
 * action: "start" | "stop" | "restart"
 * service: nome canonico dal catalogo (es. "mcp-core", "nexus-gateway").
 * L'allowlist di controllo e' il campo `controllable` del catalogo DB
 * (`system.services_catalog`, migrazione 0541), applicata dal backend.
 */
import { NextResponse } from "next/server";

const MCP_CORE_URL =
  process.env.MCP_CORE_URL || process.env.CORE_SERVICE_URL || "http://localhost:4000";

type Params = { service: string; action: string };

export async function POST(
  request: Request,
  { params }: { params: Promise<Params> }
) {
  const { service, action } = await params;
  const targetUrl = `${MCP_CORE_URL}/api/system/services/${encodeURIComponent(
    service
  )}/${encodeURIComponent(action)}`;

  try {
    const cookieHeader = request.headers.get("cookie") ?? "";
    const response = await fetch(targetUrl, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        ...(cookieHeader ? { Cookie: cookieHeader } : {}),
      },
    });
    const data = await response.json();
    return NextResponse.json(data, { status: response.status });
  } catch (error) {
    console.error(`[API Proxy] POST system/services/${service}/${action} error:`, error);
    return NextResponse.json(
      { ok: false, service, action, stderr: "mcp-core non raggiungibile" },
      { status: 503 }
    );
  }
}
