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

const CORE_URL =
  process.env.CORE_SERVICE_URL ||
  process.env.MCP_CORE_URL ||
  process.env.BACKEND_URL ||
  "http://127.0.0.1:4000";

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

  // Propaga cookie di autenticazione e Last-Event-ID per replay SSE
  const headers: Record<string, string> = { Accept: "text/event-stream" };
  const cookie = request.headers.get("cookie");
  if (cookie) headers["cookie"] = cookie;
  const lastEventId = request.headers.get("last-event-id");
  if (lastEventId) headers["last-event-id"] = lastEventId;

  let upstream: Response;
  try {
    upstream = await fetch(backendUrl, { headers, cache: "no-store" });
  } catch (err) {
    console.error("[event-stream proxy] backend non raggiungibile:", err);
    return new Response(
      JSON.stringify({ error: "Backend non raggiungibile" }),
      { status: 502, headers: { "Content-Type": "application/json" } },
    );
  }

  if (!upstream.ok) {
    // Propaga lo status del backend (es. 401, 404, 500)
    const body = await upstream.text().catch(() => "");
    return new Response(body || JSON.stringify({ error: `Backend: ${upstream.status}` }), {
      status: upstream.status,
      headers: { "Content-Type": upstream.headers.get("content-type") || "application/json" },
    });
  }

  if (!upstream.body) {
    return new Response(
      JSON.stringify({ error: "Backend ha risposto senza body" }),
      { status: 502, headers: { "Content-Type": "application/json" } },
    );
  }

  // Pipe lo stream SSE dal backend al client senza buffering.
  // Usiamo un ReadableStream esplicito per garantire che ogni chunk
  // venga inoltrato immediatamente (flush-on-enqueue).
  const reader = upstream.body.getReader();
  const stream = new ReadableStream({
    async pull(controller) {
      try {
        const { done, value } = await reader.read();
        if (done) {
          controller.close();
          return;
        }
        controller.enqueue(value);
      } catch {
        controller.close();
      }
    },
    cancel() {
      reader.cancel().catch(() => {});
    },
  });

  return new Response(stream, {
    status: 200,
    headers: {
      "Content-Type": "text/event-stream",
      "Cache-Control": "no-cache, no-transform",
      "Connection": "keep-alive",
      // Disabilita buffering in proxy intermedi (nginx, etc.)
      "X-Accel-Buffering": "no",
    },
  });
}
