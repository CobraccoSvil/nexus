---
id: adr-0029-gemini-3-fantasma-discovery-vs-inference
kind: adr
title: "ADR 0029 - Modelli fantasma Google: discovery list vs inference reale"
slug: 0029-gemini-3-fantasma-discovery-vs-inference
tags:
  - adr
  - routing
  - catalog
  - provider
  - google
  - vertex
  - model-selection
  - data-residency
auto_generated: false
nexus_meta_version: 1
---

# ADR 0029 - Modelli fantasma Google: discovery list vs inference reale

## Stato

Accepted - 2026-06-09. Nessuna modifica di codice ne' di schema: il sistema e'
gia' self-healing per design.

## Contesto

Sospetto operativo: il routing giudicava Google inferiore o usava modelli vecchi.
Il pool capace Google si fermava a `gemini-2.5-pro` mentre esistono i Gemini 3.x.
La domanda era se mancasse un upgrade automatico o se ci fosse un gate troppo
aggressivo.

## Indagine

Catena di causa, tre livelli.

1. **Region Vertex.** Il backend Google e' Vertex AI (Service Account enterprise),
   setting `google_vertex_location` = `europe-west4`. La discovery `models.list()`
   su `europe-west4` ritorna 19 modelli, ZERO gemini-3.x. Su `us-central1` ritorna
   130 modelli, inclusi tutti i gemini-3.x (`gemini-3-pro-preview`,
   `gemini-3.1-pro-preview`, `gemini-3.5-flash`, `gemini-3-flash-preview`, ecc.).
   Su `global` ~20 modelli con parte dei 3.x.

2. **La lista non e' la verita'.** Probe di inference reale (`generate_content`) su
   `us-central1` per `gemini-3-pro-preview`, `gemini-3.1-pro-preview`,
   `gemini-3.5-flash`, `gemini-3-flash-preview` ha ritornato per TUTTI 404
   NOT_FOUND ("Publisher Model
   projects/nexus-492307/locations/us-central1/publishers/google/models/<m> was
   not found"). I gemini-3 sono preview gated / non-GA: il progetto Vertex
   `nexus-492307` NON e' allowlistato per eseguirli. Sono i "modelli fantasma":
   esposti nella discovery list ma non eseguibili.

3. **Il gate corretto era gia' attivo.** `nexus_model_selection_policy` per il
   provider google ha `allowed_patterns = {^gemini-2\.5}` e `denied_patterns` che
   escludono image/tts/embedding/imagen/gemini-1/gemma/aqa. Questa allowlist
   bloccava deliberatamente i 3.x. Il codice e' coerente:
   `model_catalog_sync.rs` -> `model_passes_selection_policy` (riga ~457; ramo
   Some re-enable ~932; probe-on-insert ~750 con
   `ProbeOnInsertResult::ModelBroken`) e `is_chat_compatible_model` (~48, nessuna
   blacklist per nome famiglia, commento esplicito ~95-105). I fantasma non
   possono entrare enabled via auto-discovery.

## Decisione

- **NON si cambia la region a `us-central1`.** Avrebbe spostato il traffico Google
  funzionante (gemini-2.5) fuori dall'UE (perdita della data-residency europea
  garantita da `europe-west4`) SENZA sbloccare i 3.x (404 in inference):
  peggioramento netto. La region resta / e' stata ripristinata a `europe-west4`.
- **NON si estende `allowed_patterns` a `^gemini-3` ne' si abilitano i 3.x a mano.**
  Abilitare modelli che ritornano 404 e' una toppa (regola H del CLAUDE.md). La
  policy `^gemini-2\.5` resta corretta.
- **Nessuna modifica di codice.** Il sistema e' gia' self-healing.

## Conseguenze

- **Stato finale Google sano**: 5 modelli `gemini-2.5` enabled su `europe-west4`
  (`gemini-2.5-pro`, `gemini-2.5-flash`, `gemini-2.5-flash-lite`, e due preview
  09-2025). Google NON e' inferiore: usa il massimo realmente eseguibile.
- **Self-healing automatico**: quando Google rendera' i gemini-3 accessibili al
  progetto `nexus-492307` su `europe-west4` (GA o allowlisting preview), il
  `catalog_sync` li scoprira', il probe-on-insert li trovera' eseguibili e li
  abilitera' automaticamente, allineando anche il `performance_tier`
  (`gemini-3-pro` -> heavy). Nessun intervento di codice necessario.
- **Path operativi per accelerare** (lato Google/GCP, non sviluppo):
  - (a) richiedere allowlisting del progetto `nexus-492307` ai preview Gemini 3 su
    console GCP/Vertex, oppure attendere la GA su `europe-west4`;
  - (b) in alternativa AI Studio (la sua API espone i 3.x in lista) ma comporta
    server US e l'inference va verificata - escluso per data-residency EU.

## Lezione generale

La discovery list di un provider e' un INDIZIO, non la verita': la verita' e'
l'account/progetto. Un modello listato puo' dare 404 in inference. Il
probe-on-insert (inference reale prima di abilitare) e' il PUNTO UNICO che
distingue i due casi. Mai abilitare un modello sulla sola presenza in lista.

## Riferimenti

- Setting `google_vertex_location`, `google_provider_backend`; tabella
  `nexus_model_selection_policy`.
- `crates/mcp-core/src/model_catalog_sync.rs` (`model_passes_selection_policy`,
  `is_chat_compatible_model`, probe-on-insert).
- Brain `/providers/google/models/live`, `brain/providers/google_provider.py`.
- ADR [[0024-capability-fonte-unica-classificazione]],
  [[0025-gestione-modelli-deterministica]].
- Regole G (unica fonte dati nel DB) e H (fix definitivi, mai toppe) del CLAUDE.md.
