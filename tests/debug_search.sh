#!/bin/bash
# Debug: verifica perche' search_knowledge_points ritorna 0 hit utili
set -e

PROJ_ID="db8da242-b29d-45b1-b267-b37030914eb7"
POINT_ID="66342cf7-1109-490e-af70-c0e10f550add"

# Recupera vettore
VEC=$(curl -s "http://localhost:6333/collections/knowledge_notes/points/${POINT_ID}?with_vector=true" \
  | python3 -c 'import json,sys; print(json.dumps(json.load(sys.stdin)["result"]["vector"]))')

echo "=== Search SENZA filter ==="
curl -s http://localhost:6333/collections/knowledge_notes/points/search \
  -X POST -H 'Content-Type: application/json' \
  -d "{\"vector\":${VEC},\"limit\":5,\"with_payload\":true}" \
  | python3 -c 'import json,sys; d=json.load(sys.stdin); r=d.get("result",[]); print(f"hits:{len(r)}");
[print(f"  score={h[\"score\"]:.3f} note={h[\"payload\"][\"note_id\"][:8]} proj={h[\"payload\"][\"project_id\"][:8]} status={h[\"payload\"][\"status\"]}") for h in r]'

echo ""
echo "=== Search CON filter project_id + status ==="
curl -s http://localhost:6333/collections/knowledge_notes/points/search \
  -X POST -H 'Content-Type: application/json' \
  -d "{\"vector\":${VEC},\"limit\":5,\"with_payload\":true,\"filter\":{\"must\":[{\"key\":\"project_id\",\"match\":{\"value\":\"${PROJ_ID}\"}},{\"key\":\"status\",\"match\":{\"any\":[\"active\",\"draft\"]}}]}}" \
  | python3 -c 'import json,sys; d=json.load(sys.stdin); r=d.get("result",[]); print(f"hits:{len(r)}");
[print(f"  score={h[\"score\"]:.3f} note={h[\"payload\"][\"note_id\"][:8]} status={h[\"payload\"][\"status\"]}") for h in r]'
