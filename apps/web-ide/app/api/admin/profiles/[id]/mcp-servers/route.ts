import { NextRequest } from "next/server";
import { proxyRequest } from "../../../_proxy";

export async function GET(
  request: NextRequest,
  { params }: { params: Promise<{ id: string }> },
) {
  const { id } = await params;
  return proxyRequest(request, `/api/admin/profiles/${id}/mcp-servers`);
}

export async function PUT(
  request: NextRequest,
  { params }: { params: Promise<{ id: string }> },
) {
  const { id } = await params;
  return proxyRequest(request, `/api/admin/profiles/${id}/mcp-servers`);
}
