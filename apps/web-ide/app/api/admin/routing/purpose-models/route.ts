/**
 * Proxy per la lista dei purpose model.
 * GET /api/admin/routing/purpose-models → mcp-core GET /api/admin/routing/purpose-models
 * Delega al punto unico proxyRequest (regola L).
 */

import { NextRequest, NextResponse } from "next/server";
import { proxyRequest } from "../../_proxy";

export async function GET(request: NextRequest): Promise<NextResponse> {
  return proxyRequest(request, "/api/admin/routing/purpose-models", "GET");
}
