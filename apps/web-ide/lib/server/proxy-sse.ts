/**
 * Proxy SSE verso mcp-core senza buffering (punto unico, regola L).
 *
 * I rewrite catch-all di next.config.ts bufferizzano le risposte long-lived:
 * agent-stream ed event-stream cadono lato client ("Connessione persa") anche
 * con mcp-core sano. server.js bypassa il problema in produzione; in `next dev`
 * servono route App Router dedicate che fanno pipe streaming reale.
 */

export const CORE_URL =
  process.env.CORE_SERVICE_URL ||
  process.env.MCP_CORE_URL ||
  process.env.BACKEND_URL ||
  "http://127.0.0.1:4000";

/** Inoltra la richiesta SSE al backend e restituisce uno stream senza buffering. */
export async function proxySseRequest(
  request: Request,
  backendUrl: string,
  logLabel: string,
): Promise<Response> {
  const headers: Record<string, string> = { Accept: "text/event-stream" };
  const cookie = request.headers.get("cookie");
  if (cookie) headers["cookie"] = cookie;
  const lastEventId = request.headers.get("last-event-id");
  if (lastEventId) headers["last-event-id"] = lastEventId;

  let upstream: Response;
  try {
    upstream = await fetch(backendUrl, { headers, cache: "no-store" });
  } catch (err) {
    console.error(`[${logLabel}] backend non raggiungibile:`, err);
    return new Response(
      JSON.stringify({ error: "Backend non raggiungibile" }),
      { status: 502, headers: { "Content-Type": "application/json" } },
    );
  }

  if (!upstream.ok) {
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
      Connection: "keep-alive",
      "X-Accel-Buffering": "no",
    },
  });
}
