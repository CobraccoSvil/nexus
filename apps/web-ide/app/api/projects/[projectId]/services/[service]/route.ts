import { NextResponse } from "next/server";

const MCP_CORE_URL = process.env.MCP_CORE_URL || "http://localhost:4000";

type Params = { projectId: string; service: string };

/** DELETE → disinstalla un servizio systemd del progetto. */
export async function DELETE(
  request: Request,
  { params }: { params: Promise<Params> }
) {
  const { projectId, service } = await params;
  const targetUrl = `${MCP_CORE_URL}/api/projects/${projectId}/services/${encodeURIComponent(service)}`;

  try {
    const cookieHeader = request.headers.get("cookie") ?? "";
    const response = await fetch(targetUrl, {
      method: "DELETE",
      headers: {
        "Content-Type": "application/json",
        ...(cookieHeader ? { Cookie: cookieHeader } : {}),
      },
    });
    const data = await response.json();
    return NextResponse.json(data, { status: response.status });
  } catch (error) {
    console.error(`[API Proxy] DELETE projects/${projectId}/services/${service} error:`, error);
    return NextResponse.json({ error: "Servizio non disponibile" }, { status: 503 });
  }
}
