#!/usr/bin/env python3
import json
import urllib.request

PROJ_ID = "db8da242-b29d-45b1-b267-b37030914eb7"
POINT_ID = "66342cf7-1109-490e-af70-c0e10f550add"
QDRANT = "http://localhost:6333"


def http_get(url):
    with urllib.request.urlopen(url, timeout=10) as r:
        return json.loads(r.read())


def http_post(url, body):
    req = urllib.request.Request(url, method="POST")
    req.add_header("Content-Type", "application/json")
    req.data = json.dumps(body).encode()
    with urllib.request.urlopen(req, timeout=10) as r:
        return json.loads(r.read())


pt = http_get(f"{QDRANT}/collections/knowledge_notes/points/{POINT_ID}?with_vector=true")
vec = pt["result"]["vector"]
print(f"vector dim: {len(vec)}")

print("\n=== NO filter ===")
r = http_post(
    f"{QDRANT}/collections/knowledge_notes/points/search",
    {"vector": vec, "limit": 5, "with_payload": True},
)
hits = r.get("result", [])
print(f"hits: {len(hits)}")
for h in hits:
    p = h["payload"]
    print(f"  score={h['score']:.3f} note={p['note_id'][:8]} status={p.get('status', '?')}")

print("\n=== project+status filter ===")
r = http_post(
    f"{QDRANT}/collections/knowledge_notes/points/search",
    {
        "vector": vec,
        "limit": 5,
        "with_payload": True,
        "filter": {
            "must": [
                {"key": "project_id", "match": {"value": PROJ_ID}},
                {"key": "status", "match": {"any": ["active", "draft"]}},
            ]
        },
    },
)
hits = r.get("result", [])
print(f"hits: {len(hits)}")
for h in hits:
    p = h["payload"]
    print(f"  score={h['score']:.3f} note={p['note_id'][:8]} status={p.get('status', '?')}")
