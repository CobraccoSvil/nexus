/**
 * Proxy per le impostazioni admin.
 * GET /api/admin/settings → mcp-core GET /api/admin/settings
 * Delega al punto unico proxyRequest (regola L).
 */

import { NextRequest, NextResponse } from "next/server";
import { proxyRequest } from "../_proxy";

export async function GET(request: NextRequest): Promise<NextResponse> {
  return proxyRequest(request, "/api/admin/settings", "GET");
}
