#!/usr/bin/env python3
import json
import urllib.request
from collections import Counter

PROJ_ID = "db8da242-b29d-45b1-b267-b37030914eb7"


def http_post(url, body):
    req = urllib.request.Request(url, method="POST")
    req.add_header("Content-Type", "application/json")
    req.data = json.dumps(body).encode()
    with urllib.request.urlopen(req, timeout=10) as r:
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
print(f"total points for project: {len(points)}")

note_ids = [p["payload"]["note_id"] for p in points]
counter = Counter(note_ids)
dupes = {k: v for k, v in counter.items() if v > 1}
print(f"unique note_ids: {len(set(note_ids))}")
print(f"duplicated note_ids: {len(dupes)}")
if dupes:
    print("first 5 dupes:")
    for k, v in list(dupes.items())[:5]:
        print(f"  {k[:8]} appears {v}x")
