import { NextRequest } from "next/server";
import { proxyRequest } from "../_proxy";

// GET /api/admin/provider-registry -> mcp-core: elenco provider del registry
// (fonte unica data-driven per la dashboard). Nessun segreto.
export async function GET(request: NextRequest) {
  return proxyRequest(request, "/api/admin/provider-registry");
}
