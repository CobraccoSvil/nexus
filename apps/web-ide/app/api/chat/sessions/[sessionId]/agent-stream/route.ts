/**
 * Proxy SSE per agent-stream di mcp-core.
 *
 * Stesso razionale di event-stream/route.ts: i rewrite Next.js bufferizzano
 * gli stream long-lived e il client vede "Connessione persa" anche con backend up.
 */

import { CORE_URL, proxySseRequest } from "../../../../../../lib/server/proxy-sse";

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

type Params = { sessionId: string };

export async function GET(
  request: Request,
  { params }: { params: Promise<Params> },
) {
  const { sessionId } = await params;
  const { searchParams } = new URL(request.url);
  const qs = searchParams.toString();
  const backendUrl = `${CORE_URL}/api/chat/sessions/${sessionId}/agent-stream${qs ? `?${qs}` : ""}`;
  return proxySseRequest(request, backendUrl, "agent-stream proxy");
}
