/* eslint-disable @typescript-eslint/no-require-imports -- custom server Next.js in CommonJS per proxy WebSocket */
// Custom Next.js server — proxies WebSocket upgrades on /neural/* to the brain service.
// Regular HTTP requests are handled normally by Next.js.
// This allows the browser to use wss://nexus.cobracco.it/neural/ws/terminal/...
// instead of ws://localhost:8001/ws/terminal/... (which is unreachable from the browser).

const { createServer } = require("http");
const { parse } = require("url");
const next = require("next");
const httpProxy = require("http-proxy");

const PORT = parseInt(process.env.PORT || "3000", 10);
const BRAIN_URL = process.env.BRAIN_URL || "http://localhost:8001";

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

app.prepare().then(() => {
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
