/* eslint-disable @typescript-eslint/no-require-imports -- custom server Next.js in CommonJS per proxy WebSocket */
// Custom Next.js server — proxies WebSocket upgrades on /neural/* to the brain service.
// Regular HTTP requests are handled normally by Next.js.
// This allows the browser to use wss://nexus.cobracco.it/neural/ws/terminal/...
// instead of ws://localhost:8001/ws/terminal/... (which is unreachable from the browser).

const { createServer } = require("http");
const { parse } = require("url");
const next = require("next");
const httpProxy = require("http-proxy");

const BRAIN_URL = process.env.BRAIN_URL || "http://localhost:8001";

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

// Proxy for WebSocket upgrades — strips the /neural prefix before forwarding
const wsProxy = httpProxy.createProxyServer({
  target: BRAIN_URL,
  ws: true,
  changeOrigin: true,
});

wsProxy.on("error", (err, req, socket) => {
  console.error("[ws-proxy] error:", err.message);
  if (socket && !socket.destroyed) socket.destroy();
});

app.prepare().then(async () => {
  const PORT = await resolveWebIdePort();
  const server = createServer((req, res) => {
    const parsedUrl = parse(req.url, true);
    const path = parsedUrl.pathname || "";

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

  // Intercept WebSocket upgrade events on /neural/* paths
  server.on("upgrade", (req, socket, head) => {
    const url = req.url || "";
    if (url.startsWith("/neural/") || url === "/neural") {
      // Strip the /neural prefix so the brain service sees /ws/terminal/...
      req.url = url.slice("/neural".length) || "/";
      console.log(`[ws-proxy] upgrade → ${BRAIN_URL}${req.url}`);
      wsProxy.ws(req, socket, head);
    } else {
      socket.destroy();
    }
  });

  server.listen(PORT, "0.0.0.0", () => {
    console.log(`> Ready on http://0.0.0.0:${PORT} (brain → ${BRAIN_URL})`);
  });
});
