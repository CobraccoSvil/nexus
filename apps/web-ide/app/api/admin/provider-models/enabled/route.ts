import { NextRequest, NextResponse } from "next/server";
import { proxyRequest } from "../../_proxy";

// PUT /api/admin/provider-models/enabled -> mcp-core: abilita/disabilita un
// modello del catalog (ai_price_catalog.is_enabled). Body {provider, model, enabled}.
export async function PUT(req: NextRequest): Promise<NextResponse> {
  return proxyRequest(req, "/api/admin/provider-models/enabled", "PUT");
}
