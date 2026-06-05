// Proxy same-origin verso la route PUBBLICA di mcp-core `/preview/:project_id/*path`.
// Scopo: servire il sito statico di un progetto HTML dallo stesso origin dell'IDE
// (http://localhost:3000/preview/<id>/...), cosi' l'utente puo' aprirlo in una nuova
// scheda senza problemi di CORS o di porta. La route mcp-core e' pubblica: NON
// inoltriamo autenticazione (nessun cookie/JWT), limitandoci a proxare il GET.

// Niente porte hardcoded: si usa l'env MCP_CORE_URL (fallback CORE_SERVICE_URL).
const MCP_CORE_URL =
  process.env.MCP_CORE_URL || process.env.CORE_SERVICE_URL || "http://localhost:4000";

type Params = { projectId: string; path?: string[] };

async function proxyPreview(
  projectId: string,
  pathSegments: string[],
  method: "GET" | "HEAD",
) {
  const subPath = pathSegments.map(encodeURIComponent).join("/");
  const targetUrl = `${MCP_CORE_URL}/preview/${encodeURIComponent(projectId)}${subPath ? `/${subPath}` : ""}`;

  try {
    // Nessun header di autenticazione: la route mcp-core /preview e' pubblica.
    const upstream = await fetch(targetUrl, { method, cache: "no-store" });

    // Per HEAD non c'e' corpo: propaga solo status e Content-Type.
    if (method === "HEAD") {
      const headers = new Headers();
      const ct = upstream.headers.get("content-type");
      if (ct) headers.set("Content-Type", ct);
      return new Response(null, { status: upstream.status, headers });
    }

    // Gestione contenuti binari (immagini, font, ecc.): si legge l'arrayBuffer
    // e si ritorna con lo stesso Content-Type/Content-Length.
    const body = await upstream.arrayBuffer();
    const headers = new Headers();
    const contentType = upstream.headers.get("content-type");
    if (contentType) headers.set("Content-Type", contentType);
    const contentLength = upstream.headers.get("content-length");
    if (contentLength) headers.set("Content-Length", contentLength);
    // Niente caching aggressivo: il contenuto statico puo' cambiare a ogni edit.
    headers.set("Cache-Control", "no-store");

    return new Response(body, { status: upstream.status, headers });
  } catch (error) {
    console.error(`[Preview Proxy] ${method} preview/${projectId}/${subPath} error:`, error);
    return new Response("Server statico non disponibile", { status: 503 });
  }
}

export async function GET(
  _request: Request,
  { params }: { params: Promise<Params> },
) {
  const { projectId, path = [] } = await params;
  return proxyPreview(projectId, path, "GET");
}

export async function HEAD(
  _request: Request,
  { params }: { params: Promise<Params> },
) {
  const { projectId, path = [] } = await params;
  return proxyPreview(projectId, path, "HEAD");
}
