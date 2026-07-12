import { NextRequest } from "next/server";
import { proxyRequest } from "../_proxy";

// GET /api/admin/provider-models[?provider=X] -> mcp-core: modelli del catalog
// INCLUSI i disabilitati (per abilitarli dalla dashboard). La querystring
// ?provider e' propagata automaticamente da proxyRequest.
export async function GET(request: NextRequest) {
  return proxyRequest(request, "/api/admin/provider-models");
}
