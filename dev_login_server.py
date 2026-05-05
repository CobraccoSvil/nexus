#!/usr/bin/env python3
"""
Mini-server dev per gestione sessioni JWT su localhost:9999
Endpoint:
  GET /dev-login       → genera token, setta cookie, redirect a frontend
  GET /insert-session  → inserisce sessione nel DB (chiamato da Next.js /api/dev-login)
"""
import http.server, hashlib, time, json, urllib.parse
import jwt  # PyJWT

DB_HOST = "localhost"
DB_PORT = 5433
DB_NAME = "nexus"
DB_USER = "nexus"
DB_PASS = "nexus"
JWT_SECRET = "b684609a5ecdf3a50377ec453308cf5ecf65eb66c393dc5790901d10386f030943fdcfab0c4ec0d92310c9277303b1baa43c05b26a962e7e2abd22be00cd4ae8"
FRONTEND_URL = "http://localhost:3002/api/dev-login"
PORT = 9999


def get_db():
    import psycopg2
    return psycopg2.connect(host=DB_HOST, port=DB_PORT, dbname=DB_NAME, user=DB_USER, password=DB_PASS)


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, fmt, *args): pass

    def do_GET(self):
        parsed = urllib.parse.urlparse(self.path)
        params = urllib.parse.parse_qs(parsed.query)

        if parsed.path == "/dev-login":
            self._handle_dev_login()
        elif parsed.path == "/insert-session":
            user_id = params.get("user_id", [""])[0]
            token_hash = params.get("hash", [""])[0]
            self._handle_insert_session(user_id, token_hash)
        else:
            self.send_response(404); self.end_headers()

    def _handle_dev_login(self):
        conn = get_db()
        cur = conn.cursor()
        cur.execute("SELECT id, email, role FROM users WHERE role='admin' LIMIT 1")
        row = cur.fetchone()
        if not row:
            self.send_response(404); self.end_headers()
            self.wfile.write(b"No admin user found"); return

        user_id, email, role = row
        exp = int(time.time()) + 86400 * 7
        token = jwt.encode({"sub": str(user_id), "role": role, "exp": exp}, JWT_SECRET, algorithm="HS256")
        token_hash = hashlib.sha256(token.encode()).hexdigest()

        cur.execute(
            "INSERT INTO sessions (user_id, token_hash, expires_at) VALUES (%s, %s, NOW() + INTERVAL '7 days') ON CONFLICT DO NOTHING",
            (str(user_id), token_hash)
        )
        conn.commit(); cur.close(); conn.close()

        # Redirect al Next.js dev-login che imposta il cookie dalla giusta origine
        self.send_response(302)
        self.send_header("Location", FRONTEND_URL)
        self.end_headers()
        print(f"[dev-login] token generato per {email}")

    def _handle_insert_session(self, user_id: str, token_hash: str):
        if not user_id or not token_hash:
            self.send_response(400); self.end_headers()
            self.wfile.write(b"Missing params"); return
        try:
            conn = get_db()
            cur = conn.cursor()
            cur.execute(
                "INSERT INTO sessions (user_id, token_hash, expires_at) VALUES (%s, %s, NOW() + INTERVAL '7 days') ON CONFLICT DO NOTHING",
                (user_id, token_hash)
            )
            conn.commit(); cur.close(); conn.close()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(b'{"ok":true}')
            print(f"[insert-session] OK hash={token_hash[:16]}...")
        except Exception as e:
            self.send_response(500); self.end_headers()
            self.wfile.write(str(e).encode())


if __name__ == "__main__":
    server = http.server.HTTPServer(("localhost", PORT), Handler)
    print(f"[dev-login-server] in ascolto su http://localhost:{PORT}")
    server.serve_forever()
