/**
 * Proxy SSE per il dispatcher event-stream di mcp-core.
 *
 * Perche' una route dedicata invece del rewrite next.config.ts?
 * - I rewrite di Next.js possono bufferizzare le risposte SSE long-lived
 *   impedendo lo streaming in tempo reale.
 * - Il Cookie di autenticazione potrebbe non essere propagato correttamente
 *   per connessioni EventSource attraverso il rewrite proxy.
 * - Una route App Router con ReadableStream garantisce streaming senza buffering.
 *
 * Questa route ha priorita' sul rewrite catch-all "/api/:path*" di next.config.ts.
 */

import { CORE_URL, proxySseRequest } from "../../../../../lib/server/proxy-sse";

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

type Params = { projectId: string };

export async function GET(
  request: Request,
  { params }: { params: Promise<Params> },
) {
  const { projectId } = await params;
  const { searchParams } = new URL(request.url);
  const qs = searchParams.toString();
  const backendUrl = `${CORE_URL}/api/projects/${projectId}/event-stream${qs ? `?${qs}` : ""}`;
  return proxySseRequest(request, backendUrl, "event-stream proxy");
}
