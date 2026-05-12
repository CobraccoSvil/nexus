import { NextRequest } from "next/server";
import { proxyToAdmin } from "../browser-bridge/_proxy_bb";

export async function GET(req: NextRequest) {
  return proxyToAdmin(req, "/api/admin/shared-directives", "GET");
}

export async function POST(req: NextRequest) {
  const body = await req.text();
  const ADMIN_URL = process.env.ADMIN_SERVICE_URL || "http://127.0.0.1:4010";
  const url = `${ADMIN_URL}/api/admin/shared-directives`;
  const h: Record<string, string> = { "Content-Type": "application/json" };
  const cookie = req.headers.get("cookie");
  if (cookie) h["cookie"] = cookie;
  const auth = req.headers.get("authorization");
  if (auth) h["authorization"] = auth;
  try {
    const r = await fetch(url, { method: "POST", headers: h, body });
    const data = await r.json();
    return Response.json(data, { status: r.status });
  } catch (err) {
    console.error("[proxy-bb] shared-directives POST", err);
    return Response.json({ error: "admin-service non raggiungibile" }, { status: 502 });
  }
}
