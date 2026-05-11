import { NextResponse } from "next/server";

const MCP_CORE_URL = process.env.MCP_CORE_URL || "http://localhost:4000";

type Params = { projectId: string };

export async function GET(
  request: Request,
  { params }: { params: Promise<Params> }
) {
  const { projectId } = await params;
  const targetUrl = `${MCP_CORE_URL}/api/projects/${projectId}/services/wizard/detect`;

  try {
    const cookieHeader = request.headers.get("cookie") ?? "";
    const response = await fetch(targetUrl, {
      method: "GET",
      headers: { "Content-Type": "application/json", ...(cookieHeader ? { Cookie: cookieHeader } : {}) },
    });
    const data = await response.json();
    return NextResponse.json(data, { status: response.status });
  } catch (error) {
    console.error(`[API Proxy] wizard/detect error:`, error);
    return NextResponse.json({ error: "Servizio non disponibile" }, { status: 503 });
  }
}
