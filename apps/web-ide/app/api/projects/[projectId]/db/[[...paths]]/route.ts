import { NextResponse } from "next/server";

const MCP_CORE_URL = process.env.MCP_CORE_URL || "http://localhost:4000";

type Params = { projectId: string; paths?: string[] };

export async function GET(
  request: Request,
  { params }: { params: Promise<Params> }
) {
  const { projectId, paths = [] } = await params;
  const subPath = paths.join("/");
  const targetUrl = `${MCP_CORE_URL}/api/projects/${projectId}/db${subPath ? `/${subPath}` : ""}`;

  try {
    const cookieHeader = request.headers.get("cookie") ?? "";
    const response = await fetch(targetUrl, {
      method: "GET",
      headers: { "Content-Type": "application/json", ...(cookieHeader ? { Cookie: cookieHeader } : {}) },
    });
    const data = await response.json();
    return NextResponse.json(data, { status: response.status });
  } catch (error) {
    console.error(`[API Proxy] GET projects/${projectId}/db/${subPath} error:`, error);
    return NextResponse.json({ error: "Servizio DB non disponibile" }, { status: 503 });
  }
}

export async function POST(
  request: Request,
  { params }: { params: Promise<Params> }
) {
  const { projectId, paths = [] } = await params;
  const subPath = paths.join("/");
  const targetUrl = `${MCP_CORE_URL}/api/projects/${projectId}/db${subPath ? `/${subPath}` : ""}`;

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
    console.error(`[API Proxy] POST projects/${projectId}/db/${subPath} error:`, error);
    return NextResponse.json({ error: "Servizio DB non disponibile" }, { status: 503 });
  }
}

export async function PUT(
  request: Request,
  { params }: { params: Promise<Params> }
) {
  const { projectId, paths = [] } = await params;
  const subPath = paths.join("/");
  const targetUrl = `${MCP_CORE_URL}/api/projects/${projectId}/db${subPath ? `/${subPath}` : ""}`;

  try {
    const cookieHeader = request.headers.get("cookie") ?? "";
    const body = await request.json().catch(() => null);
    const response = await fetch(targetUrl, {
      method: "PUT",
      headers: { "Content-Type": "application/json", ...(cookieHeader ? { Cookie: cookieHeader } : {}) },
      body: body ? JSON.stringify(body) : undefined,
    });
    const data = await response.json();
    return NextResponse.json(data, { status: response.status });
  } catch {
    return NextResponse.json({ error: "Servizio DB non disponibile" }, { status: 503 });
  }
}

export async function DELETE(
  request: Request,
  { params }: { params: Promise<Params> }
) {
  const { projectId, paths = [] } = await params;
  const subPath = paths.join("/");
  const targetUrl = `${MCP_CORE_URL}/api/projects/${projectId}/db${subPath ? `/${subPath}` : ""}`;

  try {
    const cookieHeader = request.headers.get("cookie") ?? "";
    const response = await fetch(targetUrl, {
      method: "DELETE",
      headers: { "Content-Type": "application/json", ...(cookieHeader ? { Cookie: cookieHeader } : {}) },
    });
    const data = await response.json();
    return NextResponse.json(data, { status: response.status });
  } catch {
    return NextResponse.json({ error: "Servizio DB non disponibile" }, { status: 503 });
  }
}
