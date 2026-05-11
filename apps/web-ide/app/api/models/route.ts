import { NextRequest, NextResponse } from "next/server";

const CORE_URL = process.env.CORE_SERVICE_URL || "http://127.0.0.1:4000";

/** GET /api/models — proxy verso mcp-core /api/models (richiede auth).
 *
 * Versione con logging diagnostico esteso per investigare 401 inattesi.
 * Quando il bug e' confermato risolto, ripristinare la versione semplice
 * (proxyRequest da ../admin/_proxy).
 */
export async function GET(request: NextRequest) {
  const cookie = request.headers.get("cookie") ?? "";
  const hasToken = /\btoken=/.test(cookie);
  const auth = request.headers.get("authorization") ?? "";

  console.log(
    `[api/models] in: cookie_len=${cookie.length} has_token_kv=${hasToken} ` +
    `auth_present=${Boolean(auth)} url=${CORE_URL}/api/models`,
  );

  const headers: Record<string, string> = { "Content-Type": "application/json" };
  if (cookie) headers["cookie"] = cookie;
  if (auth) headers["authorization"] = auth;

  try {
    const response = await fetch(`${CORE_URL}/api/models`, { method: "GET", headers });
    const bodyText = await response.text();
    const preview = bodyText.length > 200 ? bodyText.slice(0, 200) + "..." : bodyText;
    console.log(
      `[api/models] out: status=${response.status} body_len=${bodyText.length} preview=${preview}`,
    );

    if (!response.ok) {
      return NextResponse.json(
        { error: `Errore mcp-core: ${response.status} ${response.statusText}` },
        { status: response.status },
      );
    }

    try {
      const data = JSON.parse(bodyText);
      return NextResponse.json(data);
    } catch (parseErr) {
      console.error(`[api/models] JSON parse error:`, parseErr);
      return NextResponse.json({ error: "Risposta non-JSON da mcp-core", body: preview }, { status: 502 });
    }
  } catch (err) {
    console.error(`[api/models] fetch error:`, err);
    return NextResponse.json({ error: "Impossibile connettersi al servizio core" }, { status: 502 });
  }
}
