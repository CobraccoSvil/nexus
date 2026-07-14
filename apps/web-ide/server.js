/* eslint-disable @typescript-eslint/no-require-imports -- custom server Next.js in CommonJS per proxy WebSocket */
// Custom Next.js server — proxies WebSocket upgrades on /neural/* to mcp-core.
// Regular HTTP requests are handled normally by Next.js.
// Il brain Python e' stato eliminato: gli endpoint neural (incluso il WS del
// terminale) sono ora ri-esposti in mcp-core (porta 4000) sotto /api/neural/*.
// Questo permette al browser di usare wss://nexus.cobracco.it/neural/ws/terminal/...
// che il proxy inoltra a mcp-core su /api/neural/ws/terminal/...

const http = require("http");
const net = require("net");
const { createServer } = http;
const { parse } = require("url");
const next = require("next");
const httpProxy = require("http-proxy");

// Porta di bind risolta ESCLUSIVAMENTE dal DB (settings.web_ide_port, regola G:
// niente env, niente default hardcoded). DB irraggiungibile -> retry 5x5s, poi
// exit; chiave assente/non valida -> exit immediato.
const DB_URL = process.env.POSTGRES_URL || process.env.DATABASE_URL || "";
async function resolveWebIdePort() {
  if (!DB_URL) {
    console.error("[web-ide] POSTGRES_URL/DATABASE_URL assente: impossibile risolvere web_ide_port (regola G).");
    process.exit(1);
  }
  const { default: postgres } = await import("postgres");
  for (let attempt = 1; attempt <= 5; attempt++) {
    const sql = postgres(DB_URL, { max: 1, idle_timeout: 5, connect_timeout: 5 });
    try {
      const rows = await sql`SELECT value FROM settings WHERE key = 'web_ide_port'`;
      await sql.end();
      const raw = rows[0] && rows[0].value ? String(rows[0].value).trim() : "";
      const port = raw ? Number(raw) : NaN;
      if (!Number.isInteger(port) || port <= 0 || port > 65535) {
        console.error(`[web-ide] settings.web_ide_port assente o non valido (${raw || "null"}). Applica la migrazione 0239 (regola G).`);
        process.exit(1);
      }
      return port;
    } catch (err) {
      await sql.end().catch(() => {});
      if (attempt === 5) {
        console.error(`[web-ide] impossibile leggere web_ide_port dal DB dopo 5 tentativi: ${err.message}`);
        process.exit(1);
      }
      await new Promise((r) => setTimeout(r, 5000));
    }
  }
  process.exit(1); // unreachable
}

const dev = process.env.NODE_ENV !== "production";
const app = next({ dev, dir: __dirname });
const handle = app.getRequestHandler();

// URL di mcp-core: stessa convenzione del fallback /api/:path* (next.config.ts)
// e del proxy SSO sotto. Niente nuova env var dedicata al brain (eliminato).
const BACKEND_URL = process.env.BACKEND_URL || "http://localhost:4000";
const BACKEND_WS = new URL(BACKEND_URL);

// Proxy DIRETTO per gli endpoint SSE (text/event-stream) verso mcp-core.
// Le rewrites di Next.js (next.config.ts -> /api/:path* -> :4000) BUFFERIZZANO
// la risposta: per gli stream SSE long-lived (agent-stream, event-stream) i
// meta_step restano nel buffer e non arrivano al browser in tempo reale; dopo
// l'evento iniziale la connessione cade lato client ("Failed to fetch") e i
// progressi del run agente non si vedono. http-proxy fa pipe streaming reale
// (selfHandleResponse=false di default) -> nessun buffering, i chunk arrivano
// appena emessi. Stesso target del fallback /api/:path* (BACKEND_URL = :4000).
const sseProxy = httpProxy.createProxyServer({
  target: BACKEND_URL,
  changeOrigin: true,
});

sseProxy.on("error", (err, req, res) => {
  console.error("[sse-proxy] error:", err.message);
  if (res && !res.headersSent && typeof res.writeHead === "function") {
    res.writeHead(502, { "content-type": "text/plain" });
  }
  if (res && typeof res.end === "function" && !res.writableEnded) res.end();
});

// Disabilita il buffering Nagle sui socket SSE: flush immediato di ogni chunk.
sseProxy.on("proxyRes", (proxyRes, req, res) => {
  if (typeof res.setHeader === "function" && !res.headersSent) {
    res.setHeader("X-Accel-Buffering", "no");
  }
  if (res.socket && typeof res.socket.setNoDelay === "function") {
    res.socket.setNoDelay(true);
  }
});

/** True per i path SSE long-lived che NON devono passare per il buffering Next.js. */
function isSseStreamPath(path) {
  return (
    path.endsWith("/agent-stream") ||
    path.endsWith("/event-stream") ||
    /\/playwright\/runs\/[^/]+\/stream$/.test(path)
  );
}

app.prepare().then(async () => {
  const PORT = await resolveWebIdePort();
  const server = createServer((req, res) => {
    const parsedUrl = parse(req.url, true);
    const path = parsedUrl.pathname || "";

    // Stream SSE → proxy diretto a mcp-core (bypassa il buffering Next.js).
    // Senza questo i progressi del run agente non arrivano al browser.
    if (isSseStreamPath(path)) {
      sseProxy.web(req, res);
      return;
    }

    // Fix MIME type for CSS files (browser cache may reference old dev-mode paths)
    if (path.endsWith(".css") || path.includes("/_next/static/css/")) {
      const originalWriteHead = res.writeHead.bind(res);
      res.writeHead = function (statusCode, headers) {
        const existing = res.getHeader("content-type") || "";
        if (!String(existing).includes("text/css")) {
          res.setHeader("content-type", "text/css; charset=UTF-8");
        }
        return originalWriteHead(statusCode, headers);
      };
    }

    handle(req, res, parsedUrl);
  });

  // Handler UNICO per gli upgrade WebSocket su /neural/* -> mcp-core.
  // Inoltro RAW (net.connect): connessione TCP verso mcp-core, request-line +
  // header riscritti a mano, poi pipe grezzo bidirezionale. La risposta
  // "HTTP/1.1 101 ..." di mcp-core attraversa il pipe UNA sola volta verso il
  // browser, che la consuma come handshake (http-proxy.ws() invece la
  // re-inoltrava come payload del primo frame -> frame malformato -> 1002).
  const upgradeHandler = (req, socket, head) => {
    const url = req.url || "";
    if (!(url.startsWith("/neural/") || url === "/neural")) {
      // In produzione non servono altri upgrade: niente HMR, niente WS interni.
      socket.destroy();
      return;
    }
    const targetPath = `/api/neural${url.slice("/neural".length) || "/"}`;
    console.log(`[ws-proxy] upgrade → ${BACKEND_URL}${targetPath}`);

    const upstream = net.connect(Number(BACKEND_WS.port), BACKEND_WS.hostname, () => {
      const headers = { ...req.headers, host: BACKEND_WS.host };
      let raw = `${req.method} ${targetPath} HTTP/1.1\r\n`;
      for (const [k, v] of Object.entries(headers)) {
        if (Array.isArray(v)) for (const vv of v) raw += `${k}: ${vv}\r\n`;
        else raw += `${k}: ${v}\r\n`;
      }
      raw += "\r\n";
      upstream.write(raw);
      if (head && head.length) upstream.write(head);
      upstream.setNoDelay(true);
      socket.setNoDelay(true);
      upstream.pipe(socket);
      socket.pipe(upstream);
    });

    const teardown = () => {
      if (!socket.destroyed) socket.destroy();
      if (!upstream.destroyed) upstream.destroy();
    };
    upstream.on("error", (err) => {
      console.error("[ws-proxy] error:", err.message);
      teardown();
    });
    socket.on("error", teardown);
    upstream.on("close", teardown);
    socket.on("close", teardown);
  };

  server.on("upgrade", upgradeHandler);

  // Next.js (o librerie WS interne) registrano un PROPRIO listener 'upgrade' al
  // primo handle() di una richiesta HTTP. Quel listener risponde con un SECONDO
  // handshake 101 anche sui path /neural/, generando un doppio 101 verso il
  // browser -> CloseFrame 1002 "Protocol Error" -> terminale in loop di
  // riconnessione. /neural/ e' gestito interamente da upgradeHandler e in
  // produzione non servono altri upgrade: blocchiamo l'aggiunta di qualunque
  // altro listener 'upgrade' cosi' il nostro resta l'unico.
  const onlyOurUpgrade = (orig) =>
    function (event, listener) {
      if (event === "upgrade" && listener !== upgradeHandler) return this;
      return orig.call(this, event, listener);
    };
  server.on = onlyOurUpgrade(server.on.bind(server));
  server.addListener = onlyOurUpgrade(server.addListener.bind(server));
  server.prependListener = onlyOurUpgrade(server.prependListener.bind(server));

  // Bind DUAL-STACK (regola H, causa radice). Prima era vincolato a "0.0.0.0" = solo
  // IPv4: ma su Windows `localhost` risolve PRIMA a ::1 (IPv6), quindi ogni richiesta
  // del browser tentava ::1:PORT, non trovava listener e pagava ~2s di timeout prima
  // di ripiegare su IPv4. Misurato: localhost 2.40s vs 127.0.0.1 0.36s -> con decine di
  // fetch per pagina la UI diventava inusabile (richieste accodate/abortite, LED
  // provider grigi, "Impossibile caricare le viste operative"). Omettendo l'host, Node
  // binda su :: quando IPv6 e' disponibile accettando ANCHE IPv4 (IPv4-mapped), con
  // fallback automatico a 0.0.0.0 dove IPv6 non c'e'.
  server.listen(PORT, () => {
    console.log(`> Ready on port ${PORT} (dual-stack, neural → ${BACKEND_URL}/api/neural)`);
  });
});
