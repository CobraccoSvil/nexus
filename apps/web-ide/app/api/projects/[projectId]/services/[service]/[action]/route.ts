import { NextResponse } from "next/server";

const MCP_CORE_URL = process.env.MCP_CORE_URL || "http://localhost:4000";

type Params = { projectId: string; service: string; action: string };

export async function POST(
  request: Request,
  { params }: { params: Promise<Params> }
) {
  const { projectId, service, action } = await params;
  const targetUrl = `${MCP_CORE_URL}/api/projects/${projectId}/services/${service}/${action}`;

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
    console.error(`[API Proxy] POST projects/${projectId}/services/${service}/${action} error:`, error);
    return NextResponse.json({ error: "Servizio non disponibile" }, { status: 503 });
  }
}
