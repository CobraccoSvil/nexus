import { type NextRequest } from "next/server";
import { proxyRequest } from "../admin/_proxy";

/** GET /api/models — proxy verso mcp-core /api/models (richiede auth) */
export async function GET(request: NextRequest) {
  return proxyRequest(request, "/api/models");
}
