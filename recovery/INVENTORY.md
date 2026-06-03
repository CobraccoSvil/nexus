# Recovery inventory

- **knowledge/ingest_run.rs**: solo edit, nessun write base (16 edit) — salvo edits
- **knowledge/auto_link.rs**: ricostruito (12697 char, 1 write, 0 edit applicati) -> recovery/files/auto_link.rs
- **knowledge/code_graph.rs**: ricostruito (10340 char, 2 write, 2 edit applicati) -> recovery/files/code_graph.rs
- **knowledge/impact.rs**: NESSUN write/edit trovato nei transcript
- **regression_gate_node.py**: ricostruito (11105 char, 2 write, 4 edit applicati) -> recovery/files/regression_gate_node.py
- files.rs: 14 edit + 1 write -> recovery/edits/files.rs.edits.json
- chat_attachments.rs: 14 edit + 1 write -> recovery/edits/chat_attachments.rs.edits.json
- orchestrator.rs: 139 edit + 0 write -> recovery/edits/orchestrator.rs.edits.json
- agent_types.rs: 18 edit + 0 write -> recovery/edits/agent_types.rs.edits.json
- indexing.rs: 8 edit + 1 write -> recovery/edits/indexing.rs.edits.json
- service.py: 30 edit + 2 write -> recovery/edits/service.py.edits.json
- graph.py: 22 edit + 1 write -> recovery/edits/graph.py.edits.json
- clarify_or_expand_node.py: 26 edit + 0 write -> recovery/edits/clarify_or_expand_node.py.edits.json
- state.py: 14 edit + 2 write -> recovery/edits/state.py.edits.json
- nodes.py: 152 edit + 1 write -> recovery/edits/nodes.py.edits.json
- google_provider.py: 42 edit + 0 write -> recovery/edits/google_provider.py.edits.json
- registry.py: 35 edit + 3 write -> recovery/edits/registry.py.edits.json
- brain_agent_client.rs: 73 edit + 0 write -> recovery/edits/brain_agent_client.rs.edits.json
- agent-meta-step-card.tsx: 6 edit + 0 write -> recovery/edits/agent-meta-step-card.tsx.edits.json
- main.rs: 169 edit + 1 write -> recovery/edits/main.rs.edits.json

## Note Fase 0 (esito estrazione)

### Ricostruiti integralmente e validati (recovery/files/)
- auto_link.rs (12875 char) — link composer M12.3
- code_graph.rs (10340 char) — parser import M13.1
- regression_gate_node.py (11219 char) — SINTASSI OK — M13.4/5

### Da completare in Fase 2 (estrazione parziale)
- ingest_run.rs: Write base assente nei transcript; 16 edit recuperati (recovery/edits/). Base gia' presente in knowledge/mod.rs:568 a 0239 -> ricostruire combinando mod.rs + edit + piano M12.1.
- impact.rs: nessun tool_use nei transcript -> ricostruire dalle specifiche M13.2 del piano (nexus_impact_brief, impact set strutturale+semantico).

### File esistenti modificati (recovery/edits/*.edits.json) — riferimento per riapplicazione guidata su main 0239
- Pesi maggiori: main.rs (169 edit), nodes.py (152), orchestrator.rs (139), brain_agent_client.rs (73), google_provider.py (42), registry.py (35+3w), service.py (30+2w).
- ATTENZIONE: gli old_string degli edit si riferiscono al codice del branch perso (0259), NON a main 0239. Vanno riadattati al contesto main. Usarli come SPECIFICA del cambiamento, non come patch applicabili alla cieca.
