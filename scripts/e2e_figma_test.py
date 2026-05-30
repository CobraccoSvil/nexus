#!/usr/bin/env python3
"""Test E2E autonomo: creazione app da allegato Figma PL.make.

Flusso (interamente dentro Nexus, nessun intervento esterno):
  1. setup progetto E2E pulito + auth (JWT cookie + riga sessions)
  2. crea chat_session
  3. POST messaggio "crea applicazione descritta nel file" + allegato PL.make
  4. consuma SSE agent-stream fino a stato terminale
  5. verifica i file creati nella project_root

Idempotente: cancella e ricrea il progetto E2E a ogni run.
"""
from __future__ import annotations
import base64
import hashlib
import json
import os
import sys
import time
import uuid
from datetime import datetime, timezone, timedelta

import jwt
import psycopg2
import requests

DB = "postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable"
MCP = "http://127.0.0.1:4000"
PL_MAKE = "/home/administrator/projects/BeautyBook/.nexus/attachments/ede4c0f5-221d-430b-8548-9a3fdb2ae0ec/PL.make"
PROJECT_NAME = "barberia-e2e"
PROJECT_SLUG = "barberia-e2e"
PROJECT_ROOT = "/home/administrator/projects/barberia-e2e"
ADMIN_EMAIL = "github@brachini.com"

def log(msg):
    print(f"[{datetime.now().strftime('%H:%M:%S')}] {msg}", flush=True)

def db():
    return psycopg2.connect(DB)

def setup_project(conn):
    cur = conn.cursor()
    # team + owner
    cur.execute("SELECT id FROM users WHERE email=%s", (ADMIN_EMAIL,))
    owner = cur.fetchone()[0]
    cur.execute("SELECT team_id FROM projects WHERE slug='beautybook' LIMIT 1")
    row = cur.fetchone()
    team = row[0] if row else None
    if team is None:
        cur.execute("SELECT id FROM teams LIMIT 1")
        team = cur.fetchone()[0]
    # cleanup progetto E2E esistente (cascade su chat_sessions/messages/attachments)
    cur.execute("DELETE FROM projects WHERE slug=%s", (PROJECT_SLUG,))
    pid = str(uuid.uuid4())
    cur.execute(
        """INSERT INTO projects (id, team_id, name, slug, default_branch, owner_user_id,
                                 visibility, obsidian_vault_name, repository_root_path, created_at)
           VALUES (%s,%s,%s,%s,'main',%s,'private',%s,%s, now())""",
        (pid, team, PROJECT_NAME, PROJECT_SLUG, owner, PROJECT_NAME, PROJECT_ROOT),
    )
    # repositories + workspaces + membership: richiesti da load_project_context
    # (JOIN) per risolvere repository_root_path e persistere gli allegati.
    cur.execute(
        """INSERT INTO repositories (id, project_id, provider, root_path, is_git_repo, current_branch, created_at)
           VALUES (%s,%s,'local',%s,FALSE,'main', now())""",
        (str(uuid.uuid4()), pid, PROJECT_ROOT),
    )
    cur.execute(
        """INSERT INTO workspaces (id, project_id, absolute_path, is_primary, created_at)
           VALUES (%s,%s,%s,TRUE, now())""",
        (str(uuid.uuid4()), pid, PROJECT_ROOT),
    )
    cur.execute(
        """INSERT INTO project_members (id, project_id, user_id, role, created_at)
           VALUES (%s,%s,%s,'owner', now()) ON CONFLICT DO NOTHING""",
        (str(uuid.uuid4()), pid, str(owner)),
    )
    conn.commit()
    os.makedirs(PROJECT_ROOT, exist_ok=True)
    log(f"progetto E2E completo creato: {pid} root={PROJECT_ROOT}")
    return pid, owner

def make_auth(conn, owner):
    cur = conn.cursor()
    cur.execute("SELECT value FROM settings WHERE key='jwt_secret'")
    secret = cur.fetchone()[0]
    exp = int((datetime.now(timezone.utc) + timedelta(hours=2)).timestamp())
    token = jwt.encode({"sub": str(owner), "role": "admin", "exp": exp}, secret, algorithm="HS256")
    if isinstance(token, bytes):
        token = token.decode()
    th = hashlib.sha256(token.encode()).hexdigest()
    cur.execute(
        "INSERT INTO sessions (id, user_id, token_hash, expires_at, created_at) VALUES (%s,%s,%s,%s, now())",
        (str(uuid.uuid4()), str(owner), th, datetime.now(timezone.utc) + timedelta(hours=2)),
    )
    conn.commit()
    log("auth pronta (JWT + sessions row)")
    return token

def create_session(conn, pid, owner):
    cur = conn.cursor()
    sid = str(uuid.uuid4())
    cur.execute(
        "INSERT INTO chat_sessions (id, project_id, user_id, title, status) VALUES (%s,%s,%s,'E2E Figma','active')",
        (sid, pid, str(owner)),
    )
    conn.commit()
    log(f"chat_session: {sid}")
    return sid

def send_message(token, sid):
    with open(PL_MAKE, "rb") as f:
        raw = f.read()
    b64 = base64.b64encode(raw).decode()
    payload = {
        "content": "Crea l'applicazione completa descritta nel file allegato. Implementala e avviala.",
        "automationMode": "automatic",
        "attachments": [{
            "name": "PL.make",
            "mimeType": "application/octet-stream",
            "sizeBytes": len(raw),
            "base64Content": b64,
        }],
    }
    r = requests.post(
        f"{MCP}/api/chat/sessions/{sid}/messages",
        json=payload,
        headers={"Cookie": f"token={token}", "Content-Type": "application/json"},
        timeout=60,
    )
    log(f"POST messaggio -> HTTP {r.status_code}")
    if r.status_code >= 400:
        log(f"BODY: {r.text[:500]}")
        return None
    data = r.json()
    return data

def consume_stream(token, sid, run_id, max_secs=600):
    url = f"{MCP}/api/chat/sessions/{sid}/agent-stream?run_id={run_id}"
    log(f"SSE stream run_id={run_id}")
    thinking, tools, final = [], [], None
    t0 = time.time()
    with requests.get(url, headers={"Cookie": f"token={token}"}, stream=True, timeout=max_secs) as r:
        evt = None
        for line in r.iter_lines(decode_unicode=True):
            if time.time() - t0 > max_secs:
                log("TIMEOUT stream"); break
            if not line:
                continue
            if line.startswith("event:"):
                evt = line[6:].strip()
            elif line.startswith("data:"):
                d = line[5:].strip()
                if evt == "agent_thinking":
                    try: thinking.append(json.loads(d).get("text","")[:120])
                    except: pass
                elif evt == "agent_step":
                    try:
                        st = json.loads(d).get("step") or {}
                        tn = st.get("toolName")
                        if tn: tools.append(tn)
                    except: pass
                elif evt == "agent_final":
                    final = d[:300]
                    log("agent_final ricevuto"); break
    return thinking, tools, final

def verify(pid):
    import subprocess
    files = []
    for root, _, fns in os.walk(PROJECT_ROOT):
        if "/.git" in root or "/node_modules" in root or "/.nexus" in root:
            continue
        for fn in fns:
            files.append(os.path.relpath(os.path.join(root, fn), PROJECT_ROOT))
    return files

def main():
    conn = db()
    pid, owner = setup_project(conn)
    token = make_auth(conn, owner)
    sid = create_session(conn, pid, owner)
    resp = send_message(token, sid)
    if not resp:
        log("FALLITO invio messaggio"); return 1
    run_id = (resp.get("agentRun") or {}).get("runId") or resp.get("runId") or resp.get("run_id")
    log(f"resp keys: {list(resp.keys())} run_id={run_id}")
    if not run_id:
        log(f"resp completa: {json.dumps(resp)[:800]}"); return 1
    thinking, tools, final = consume_stream(token, sid, run_id)
    log(f"=== RISULTATO ===")
    log(f"thinking steps: {len(thinking)}")
    for t in thinking[:8]: log(f"  THINK: {t}")
    log(f"tool calls ({len(tools)}): {tools[:30]}")
    log(f"final: {final}")
    files = verify(pid)
    log(f"file creati nel progetto ({len(files)}): {files[:40]}")
    return 0

if __name__ == "__main__":
    sys.exit(main())
