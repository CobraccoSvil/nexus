import { type NextRequest, NextResponse } from "next/server";
import { proxyRequest } from "../_proxy";

/** GET /api/admin/watchdog-status — proxy verso mcp-core */
export async function GET(req: NextRequest): Promise<NextResponse> {
  return proxyRequest(req, "/api/admin/watchdog-status", "GET");
}
