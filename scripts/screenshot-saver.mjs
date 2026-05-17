import http from "http";
import fs from "fs";
import path from "path";

const dir = "/home/administrator/ideai/apps/web-ide/public/screenshots";
fs.mkdirSync(dir, { recursive: true });

const srv = http.createServer((req, res) => {
  res.setHeader("Access-Control-Allow-Origin", "*");
  res.setHeader("Access-Control-Allow-Methods", "POST, OPTIONS");
  res.setHeader("Access-Control-Allow-Headers", "Content-Type");

  if (req.method === "OPTIONS") {
    res.writeHead(204);
    res.end();
    return;
  }

  if (req.method === "POST") {
    let body = "";
    req.on("data", (c) => (body += c));
    req.on("end", () => {
      try {
        const { name, dataUrl } = JSON.parse(body);
        const base64 = dataUrl.replace(/^data:image\/\w+;base64,/, "");
        const buf = Buffer.from(base64, "base64");
        const fp = path.join(dir, name + ".jpg");
        fs.writeFileSync(fp, buf);
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ ok: true, size: buf.length, path: fp }));
        console.log("Salvato:", fp, buf.length, "bytes");
      } catch (e) {
        res.writeHead(500, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ error: e.message }));
      }
    });
  } else {
    res.writeHead(200);
    res.end("screenshot-saver ready");
  }
});

srv.listen(9876, () => console.log("Screenshot saver su porta 9876"));
setTimeout(() => {
  srv.close();
  process.exit(0);
}, 180000);
