import { NextRequest } from "next/server";

const ADMIN_URL = process.env.ADMIN_SERVICE_URL || "http://127.0.0.1:4010";

export async function POST(
  req: NextRequest,
  { params }: { params: Promise<{ key: string }> },
) {
  const { key } = await params;
  const url = `${ADMIN_URL}/api/admin/shared-directives/${encodeURIComponent(key)}/toggle`;
  const h: Record<string, string> = { "Content-Type": "application/json" };
  const cookie = req.headers.get("cookie");
  if (cookie) h["cookie"] = cookie;
  const auth = req.headers.get("authorization");
  if (auth) h["authorization"] = auth;
  try {
    const r = await fetch(url, { method: "POST", headers: h });
    const data = await r.json();
    return Response.json(data, { status: r.status });
  } catch (err) {
    console.error("[proxy-bb] shared-directives toggle", err);
    return Response.json({ error: "admin-service non raggiungibile" }, { status: 502 });
  }
}
