#!/usr/bin/env python3
import json
import urllib.request

PROJ_ID = "db8da242-b29d-45b1-b267-b37030914eb7"


def http_post(url, body):
    req = urllib.request.Request(url, method="POST")
    req.add_header("Content-Type", "application/json")
    req.data = json.dumps(body).encode()
    with urllib.request.urlopen(req, timeout=10) as r:
        return json.loads(r.read())


def http_get(url):
    with urllib.request.urlopen(url, timeout=10) as r:
        return json.loads(r.read())


r = http_post(
    "http://localhost:6333/collections/knowledge_notes/points/scroll",
    {
        "filter": {"must": [{"key": "project_id", "match": {"value": PROJ_ID}}]},
        "limit": 100,
        "with_payload": True,
        "with_vector": False,
    },
)
points = r["result"]["points"]
print(f"points totali progetto: {len(points)}")

total_links = 0
for pt in points[:5]:
    point_id = pt["id"]
    note_id = pt["payload"]["note_id"]
    pd = http_get(
        f"http://localhost:6333/collections/knowledge_notes/points/{point_id}?with_vector=true"
    )
    vec = pd["result"]["vector"]
    sr = http_post(
        "http://localhost:6333/collections/knowledge_notes/points/search",
        {
            "vector": vec,
            "limit": 10,
            "with_payload": True,
            "filter": {
                "must": [
                    {"key": "project_id", "match": {"value": PROJ_ID}},
                    {"key": "status", "match": {"any": ["active", "draft"]}},
                ]
            },
        },
    )
    hits = sr["result"]
    distinct_others = [
        h for h in hits if h["payload"]["note_id"] != note_id and h["score"] >= 0.45
    ]
    print(f"note={note_id[:8]}: {len(hits)} hits, {len(distinct_others)} >=0.45")
    for h in hits[:5]:
        same = "(self)" if h["payload"]["note_id"] == note_id else ""
        print(f"  {h['score']:.3f} {h['payload']['note_id'][:8]} {same}")
    total_links += len(distinct_others)

print(f"\nTotale link attesi per prime 5 note: {total_links}")
