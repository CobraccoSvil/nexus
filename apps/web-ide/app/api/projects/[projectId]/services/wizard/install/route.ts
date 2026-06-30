import { NextResponse } from "next/server";

const MCP_CORE_URL = process.env.MCP_CORE_URL || "http://localhost:4000";

type Params = { projectId: string };

export async function POST(
  request: Request,
  { params }: { params: Promise<Params> }
) {
  const { projectId } = await params;
  const targetUrl = `${MCP_CORE_URL}/api/projects/${projectId}/services/wizard/install`;

  try {
    const cookieHeader = request.headers.get("cookie") ?? "";
    const body = await request.json().catch(() => null);
    const response = await fetch(targetUrl, {
      method: "POST",
      headers: { "Content-Type": "application/json", ...(cookieHeader ? { Cookie: cookieHeader } : {}) },
      body: body ? JSON.stringify(body) : undefined,
    });
    const data = await response.json();
    return NextResponse.json(data, { status: response.status });
  } catch (error) {
    console.error(`[API Proxy] wizard/install error:`, error);
    return NextResponse.json({ error: "Servizio non disponibile" }, { status: 503 });
  }
}
