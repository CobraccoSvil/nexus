---
id: aecd59a9-17ce-456f-997b-7d479d8fb623
kind: architecture
title: Brain Python
slug: brain-python
tags:
  - architecture
  - python
source_commit: ca6caf007c84eada35e86b205fa30ce7e3c3e7a7
source_files:
  - brain/
auto_generated: true
created_at: 2026-05-23T07:19:16Z
updated_at: 2026-05-23T11:19:43Z
nexus_meta_version: 1
---

Mappa modulare di `brain/` (Python + FastAPI + LangGraph). Generato automaticamente.

Vedi anche: [[crates-rust]], [[overview]], [[multi-provider-routing]], [[nexus-architetturale]].

## Top-level modules

### `agents/`

Modulo agenti LangGraph per Nexus Neural Core.

### `documents/`

Document generation module for Nexus.

### `embeddings/`

_(senza docstring)_

### `grpc_clients/`

Client gRPC del brain verso i servizi Rust (mcp-core).

### `grpc_server/`

Neural core HTTP bootstrap.

### `memory/`

Modulo di apprendimento persistente per Nexus (PostgreSQL-backed).

### `nexus_memory/`

_(modulo senza README ne docstring)_

### `providers/`

_(senza docstring)_

### `redaction/`

Client Python per Microsoft Presidio (analyzer + anonymizer).

Architettura (hybrid profile, vedi `infra/docker/docker-compose.onprem.yml`):

  brain (questo modulo) ──HTTP──> presidio-analyzer:5002    (rileva PII)
                       ──HTTP──> presidio-anonymizer:5001  (oscura PII)

Uso:

  from brain.redaction import redact_text, RedactionResult

  result = await redact_text(
      "Mario Rossi vive a Roma, email mario@example.com",
      language="it",
      entities=["PERSON", "EMAIL_ADDRESS", "LOCATION"],
  )
  print(result.anonymized)  # "<PERSON> vive a <LOCATION>, email <EMAIL_ADDRESS>"
  print(result.entities)    # [{"type": "PERSON", "start": 0, "end": 11}, ...]

CLAUDE.md §G: gli URL Presidio sono letti da env / DB settings.
Niente fallback hardcoded a localhost.

### `router/`

_(senza docstring)_

### `tests/`

_(senza docstring)_

### `utils/`

_(senza docstring)_

