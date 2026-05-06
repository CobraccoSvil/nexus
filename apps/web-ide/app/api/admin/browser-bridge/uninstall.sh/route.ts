import { NextRequest, NextResponse } from "next/server";
import { proxyToAdmin } from "../_proxy_bb";

export async function GET(req: NextRequest): Promise<NextResponse> {
  return proxyToAdmin(req, "/api/admin/browser-bridge/uninstall.sh");
}

