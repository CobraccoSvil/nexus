---
id: 3930f6e9-c6b4-4e91-9f6c-9ccd92edca50
kind: changelog
title: "docs: aggiorna meta-vault (changelog + architettura) della sessione code-wiki"
slug: docs-aggiorna-meta-vault-changelog-architettura-della-sessione-code-wiki
tags:
  - changelog
source_commit: 4fc41379a630d9928fb3e312f450069a726a2e49
source_files:
  - docs/.nexus-vault/api/rest-endpoints.md
  - docs/.nexus-vault/api/settings-keys.md
  - docs/.nexus-vault/architecture/brain-python.md
  - docs/.nexus-vault/architecture/crates-rust.md
  - docs/.nexus-vault/architecture/frontend-nextjs.md
  - docs/.nexus-vault/changelog/2026/2026-06-01-featlive-meta-step-tool-executed-live-in-chat-progresso-per-ogni-tool.md
  - docs/.nexus-vault/changelog/2026/2026-06-01-featm0-ricostruisci-migrazioni-provider-capabilities-settings-layer.md
  - docs/.nexus-vault/changelog/2026/2026-06-01-featm0m3-ricostruisci-models-capability-loader-fondamenta-provider.md
  - docs/.nexus-vault/changelog/2026/2026-06-01-featm1-ricostruisci-tool-unification-registry-translator-validator.md
  - docs/.nexus-vault/changelog/2026/2026-06-01-featm131-ricostruisci-parser-import-code-graph-code-graphrs.md
  - docs/.nexus-vault/changelog/2026/2026-06-01-featm133-test-mapping-per-naming-nel-code-graph.md
  - docs/.nexus-vault/changelog/2026/2026-06-01-featm134m135-regression-gate-node-innesto-nel-grafo.md
  - docs/.nexus-vault/changelog/2026/2026-06-01-featm3m4m5-adapter-base-con-helper-capability-driven.md
  - docs/.nexus-vault/changelog/2026/2026-06-01-featm7m12m13m14m15-ricostruisci-fondazione-db-del-piano.md
  - docs/.nexus-vault/changelog/2026/2026-06-01-fixvertex-finish-reason-esplicito-su-output-vuoto-fallback-informato.md
  - docs/.nexus-vault/changelog/2026/2026-06-01-fixvertex-thinking-budget-cap-per-evitare-malformed-function-call.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-featadmin-fase-g-componenti-condivisi-admin-pagina-esempio-refactorata.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-featast-tree-sitter-simboli-precisi-call-graph-nella-code-wiki-ibrido.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-featast-w1-code-wiki-parser-mcp-ast-language-agnostic-no-piu-limite-di-linguaggi.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-featfasea-cabla-i-provider-sul-layer-capability-soft-failure-nel-fallback.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-featfaseb1-popola-il-code-graph-durante-lindicizzazione.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-featfaseb2-auto-commit-salta-su-regression-gate-bloccato-m135.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-featide-navigazione-codice-doc-pulsante-doc-nelleditor-apre-la-code-wiki-del-fil.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-featkb-completa-lifecycle-note-m14-deprecazione-su-correzione-archiviazione-acti.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-featkb-consultazione-on-demand-tool-code-doc-push-ridotto-a-indice-link-wiki-dec.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-featkb-w2w4-code-wiki-doc-ai-per-file-language-agnostic-rendering-markdown.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-featkb-w3w5-code-wiki-diagrammi-mermaid-uso-autorevole-della-wiki-nelle-chat.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-featkb-w4-polish-tab-code-wiki-con-navigazione-ad-albero-filemodulo.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-featm12-ingest-run-auto-link-kb-hook-post-run.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-featm126-filtri-per-kind-nel-knowledge-panel.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-featm132-impactrs-forward-closure-test-selection-nexus-impact-brief.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-featm141m143-hook-promote-draft-active-flag-context-stale-note.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-featm144-intake-request-aware-stato-implementazione-conferma-anche-in-auto.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-featm151-progresso-todo-live-in-chat-evento-todoupdated-planchecklist.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-featm16-discovery-first-primo-turno-rust-campo-state-discovered.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-featm16-merge-tool-scoperti-come-native-parte-python-completa-discovery-first.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-featm16-pattern-search-inject-native-via-validazione-tool-in-list.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-featproviders-fase-f-ttl-cache-dei-loader-provider-db-driven-hardcode-sweep.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-featrouting-m7-q-value-salute-provider-per-intent-con-cooldown-nel-fallback.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-feattodo-completa-m15-evento-planupdated-edit-manuale-todo-backlog-cross-run.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-fixbilling-elimina-doppia-contabilizzazione-usage-e-popola-run-id-nel-ledger.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-fixcooldown-ttl-billing-cooldown-db-driven-regola-g.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-fixkb-la-nota-di-un-run-contiene-anche-la-risposta-dellai-non-solo-la-richiesta.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-fixkb-promuovi-le-note-draft-a-ogni-run-completato-non-solo-con-summary-ingeribi.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-fixm14m16-lifecycle-note-draft-active-intake-request-aware-riabilita-discovery-f.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-fixm4m16-soft-failure-solo-a-inizio-run-disabilita-discovery-first.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-fixprojects-root-corretto-quando-il-progetto-e-dentro-un-repo-git-piu-grande.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-fixprovider-probe-then-reenable-e-tempi-di-healthcooldown-db-driven.md
  - docs/.nexus-vault/changelog/2026/2026-06-02-fixterminal-confina-il-terminale-alla-root-del-progetto-e-ferma-il-loop-di-ricon.md
  - docs/.nexus-vault/concepts/auto-fix-workflow.md
  - docs/.nexus-vault/concepts/change-drafter.md
  - docs/.nexus-vault/concepts/glossario.md
  - docs/.nexus-vault/concepts/isolamento-progetti.md
  - docs/.nexus-vault/concepts/knowledge-base-funzionamento.md
  - docs/.nexus-vault/concepts/meta-vault-architettura.md
  - docs/.nexus-vault/concepts/multi-provider-routing.md
  - docs/.nexus-vault/concepts/nexus-architetturale.md
  - docs/.nexus-vault/concepts/nexus-funzionale.md
  - docs/.nexus-vault/concepts/pattern-learning-worker.md
  - docs/.nexus-vault/concepts/pattern-mcp-tool.md
  - docs/.nexus-vault/concepts/routing-matrix.md
  - docs/.nexus-vault/concepts/sub-agents-claude-code.md
  - docs/.nexus-vault/schema/migrations-log.md
  - docs/.nexus-vault/schema/postgres-tables.md
  - docs/.nexus-vault/schema/qdrant-collections.md
auto_generated: true
created_at: 2026-06-02T15:21:29Z
updated_at: 2026-06-02T15:21:29Z
nexus_meta_version: 1
---

# docs: aggiorna meta-vault (changelog + architettura) della sessione code-wiki

**Commit**: `4fc41379a630d9928fb3e312f450069a726a2e49` (2026-06-02 15:21 UTC)

**Significance**: 0.45

## File toccati

- `docs/.nexus-vault/api/rest-endpoints.md`
- `docs/.nexus-vault/api/settings-keys.md`
- `docs/.nexus-vault/architecture/brain-python.md`
- `docs/.nexus-vault/architecture/crates-rust.md`
- `docs/.nexus-vault/architecture/frontend-nextjs.md`
- `docs/.nexus-vault/changelog/2026/2026-06-01-featlive-meta-step-tool-executed-live-in-chat-progresso-per-ogni-tool.md`
- `docs/.nexus-vault/changelog/2026/2026-06-01-featm0-ricostruisci-migrazioni-provider-capabilities-settings-layer.md`
- `docs/.nexus-vault/changelog/2026/2026-06-01-featm0m3-ricostruisci-models-capability-loader-fondamenta-provider.md`
- `docs/.nexus-vault/changelog/2026/2026-06-01-featm1-ricostruisci-tool-unification-registry-translator-validator.md`
- `docs/.nexus-vault/changelog/2026/2026-06-01-featm131-ricostruisci-parser-import-code-graph-code-graphrs.md`
- `docs/.nexus-vault/changelog/2026/2026-06-01-featm133-test-mapping-per-naming-nel-code-graph.md`
- `docs/.nexus-vault/changelog/2026/2026-06-01-featm134m135-regression-gate-node-innesto-nel-grafo.md`
- `docs/.nexus-vault/changelog/2026/2026-06-01-featm3m4m5-adapter-base-con-helper-capability-driven.md`
- `docs/.nexus-vault/changelog/2026/2026-06-01-featm7m12m13m14m15-ricostruisci-fondazione-db-del-piano.md`
- `docs/.nexus-vault/changelog/2026/2026-06-01-fixvertex-finish-reason-esplicito-su-output-vuoto-fallback-informato.md`
- `docs/.nexus-vault/changelog/2026/2026-06-01-fixvertex-thinking-budget-cap-per-evitare-malformed-function-call.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-featadmin-fase-g-componenti-condivisi-admin-pagina-esempio-refactorata.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-featast-tree-sitter-simboli-precisi-call-graph-nella-code-wiki-ibrido.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-featast-w1-code-wiki-parser-mcp-ast-language-agnostic-no-piu-limite-di-linguaggi.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-featfasea-cabla-i-provider-sul-layer-capability-soft-failure-nel-fallback.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-featfaseb1-popola-il-code-graph-durante-lindicizzazione.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-featfaseb2-auto-commit-salta-su-regression-gate-bloccato-m135.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-featide-navigazione-codice-doc-pulsante-doc-nelleditor-apre-la-code-wiki-del-fil.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-featkb-completa-lifecycle-note-m14-deprecazione-su-correzione-archiviazione-acti.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-featkb-consultazione-on-demand-tool-code-doc-push-ridotto-a-indice-link-wiki-dec.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-featkb-w2w4-code-wiki-doc-ai-per-file-language-agnostic-rendering-markdown.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-featkb-w3w5-code-wiki-diagrammi-mermaid-uso-autorevole-della-wiki-nelle-chat.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-featkb-w4-polish-tab-code-wiki-con-navigazione-ad-albero-filemodulo.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-featm12-ingest-run-auto-link-kb-hook-post-run.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-featm126-filtri-per-kind-nel-knowledge-panel.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-featm132-impactrs-forward-closure-test-selection-nexus-impact-brief.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-featm141m143-hook-promote-draft-active-flag-context-stale-note.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-featm144-intake-request-aware-stato-implementazione-conferma-anche-in-auto.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-featm151-progresso-todo-live-in-chat-evento-todoupdated-planchecklist.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-featm16-discovery-first-primo-turno-rust-campo-state-discovered.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-featm16-merge-tool-scoperti-come-native-parte-python-completa-discovery-first.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-featm16-pattern-search-inject-native-via-validazione-tool-in-list.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-featproviders-fase-f-ttl-cache-dei-loader-provider-db-driven-hardcode-sweep.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-featrouting-m7-q-value-salute-provider-per-intent-con-cooldown-nel-fallback.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-feattodo-completa-m15-evento-planupdated-edit-manuale-todo-backlog-cross-run.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-fixbilling-elimina-doppia-contabilizzazione-usage-e-popola-run-id-nel-ledger.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-fixcooldown-ttl-billing-cooldown-db-driven-regola-g.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-fixkb-la-nota-di-un-run-contiene-anche-la-risposta-dellai-non-solo-la-richiesta.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-fixkb-promuovi-le-note-draft-a-ogni-run-completato-non-solo-con-summary-ingeribi.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-fixm14m16-lifecycle-note-draft-active-intake-request-aware-riabilita-discovery-f.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-fixm4m16-soft-failure-solo-a-inizio-run-disabilita-discovery-first.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-fixprojects-root-corretto-quando-il-progetto-e-dentro-un-repo-git-piu-grande.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-fixprovider-probe-then-reenable-e-tempi-di-healthcooldown-db-driven.md`
- `docs/.nexus-vault/changelog/2026/2026-06-02-fixterminal-confina-il-terminale-alla-root-del-progetto-e-ferma-il-loop-di-ricon.md`
- `docs/.nexus-vault/concepts/auto-fix-workflow.md`
- `docs/.nexus-vault/concepts/change-drafter.md`
- `docs/.nexus-vault/concepts/glossario.md`
- `docs/.nexus-vault/concepts/isolamento-progetti.md`
- `docs/.nexus-vault/concepts/knowledge-base-funzionamento.md`
- `docs/.nexus-vault/concepts/meta-vault-architettura.md`
- `docs/.nexus-vault/concepts/multi-provider-routing.md`
- `docs/.nexus-vault/concepts/nexus-architetturale.md`
- `docs/.nexus-vault/concepts/nexus-funzionale.md`
- `docs/.nexus-vault/concepts/pattern-learning-worker.md`
- `docs/.nexus-vault/concepts/pattern-mcp-tool.md`
- `docs/.nexus-vault/concepts/routing-matrix.md`
- `docs/.nexus-vault/concepts/sub-agents-claude-code.md`
- `docs/.nexus-vault/schema/migrations-log.md`
- `docs/.nexus-vault/schema/postgres-tables.md`
- `docs/.nexus-vault/schema/qdrant-collections.md`

## Cosa cambia

docs: aggiorna meta-vault (changelog + architettura) della sessione code-wiki

## Riferimenti

- Vedi diff git: `git show 4fc41379a630d9928fb3e312f450069a726a2e49`

## Documenti correlati

- [[qdrant-collections]]
- [[knowledge-base-funzionamento]]
- [[multi-provider-routing]]
- [[routing-matrix]]
