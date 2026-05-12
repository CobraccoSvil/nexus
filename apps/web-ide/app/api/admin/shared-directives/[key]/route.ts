import { NextRequest } from "next/server";

const ADMIN_URL = process.env.ADMIN_SERVICE_URL || "http://127.0.0.1:4010";

function fwdHeaders(req: NextRequest): Record<string, string> {
  const h: Record<string, string> = { "Content-Type": "application/json" };
  const cookie = req.headers.get("cookie");
  if (cookie) h["cookie"] = cookie;
  const auth = req.headers.get("authorization");
  if (auth) h["authorization"] = auth;
  return h;
}

export async function GET(
  req: NextRequest,
  { params }: { params: Promise<{ key: string }> },
) {
  const { key } = await params;
  const url = `${ADMIN_URL}/api/admin/shared-directives/${encodeURIComponent(key)}`;
  try {
    const r = await fetch(url, { headers: fwdHeaders(req) });
    const data = await r.json();
    return Response.json(data, { status: r.status });
  } catch (err) {
    console.error("[proxy-bb] shared-directives GET", err);
    return Response.json({ error: "admin-service non raggiungibile" }, { status: 502 });
  }
}

export async function PUT(
  req: NextRequest,
  { params }: { params: Promise<{ key: string }> },
) {
  const { key } = await params;
  const body = await req.text();
  const url = `${ADMIN_URL}/api/admin/shared-directives/${encodeURIComponent(key)}`;
  try {
    const r = await fetch(url, { method: "PUT", headers: fwdHeaders(req), body });
    const data = await r.json();
    return Response.json(data, { status: r.status });
  } catch (err) {
    console.error("[proxy-bb] shared-directives PUT", err);
    return Response.json({ error: "admin-service non raggiungibile" }, { status: 502 });
  }
}

export async function DELETE(
  req: NextRequest,
  { params }: { params: Promise<{ key: string }> },
) {
  const { key } = await params;
  const url = `${ADMIN_URL}/api/admin/shared-directives/${encodeURIComponent(key)}`;
  try {
    const r = await fetch(url, { method: "DELETE", headers: fwdHeaders(req) });
    const data = await r.json();
    return Response.json(data, { status: r.status });
  } catch (err) {
    console.error("[proxy-bb] shared-directives DELETE", err);
    return Response.json({ error: "admin-service non raggiungibile" }, { status: 502 });
  }
}
