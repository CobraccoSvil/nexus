# Tech debt: dead code

Metrica e baseline del codice morto cross-linguaggio, parte della bonifica
2026-06-11 (vedi anche ADR 0031 per l'audit settings e `tech-debt-dup.md`
per la duplicazione).

## Come si misura

```bash
bash scripts/dead-code-report.sh                  # misura + gate ratchet
bash scripts/dead-code-report.sh --report-only    # solo misura
bash scripts/dead-code-report.sh --update-baseline  # riallinea (mai al rialzo)
```

Rilevatori:
- **Rust**: warning `dead_code` del compilatore su `CARGO_TARGET_DIR` separata
  (storicamente erano invisibili: `--cap-lints allow` in `.cargo/config.toml`,
  presente dal bootstrap, cappava TUTTI i lint — rimosso l'11/06/2026 dopo la
  bonifica) + `cargo machete` per le dipendenze.
- **TypeScript**: `knip` su `apps/web-ide` (config in `apps/web-ide/knip.json`:
  gli spec Playwright sono entrypoint, non file morti).
- **Python**: `vulture brain/ --min-confidence 80` con whitelist dei falsi
  positivi da framework in `brain/.vulture_whitelist.py` (checkpointer
  LangGraph, servicer gRPC, handler FastAPI, campi Pydantic/AgentState).

Il gate ratchet (`.dead-code-baseline.json`) ammette solo conteggi in discesa,
come il gate jscpd. On-demand + CI; NON in pre-commit (troppo lento).

## Bonifica 2026-06-11 (baseline iniziale)

Triage adversariale multi-agente su 170 warning Rust + 38 file TS + 105
funzioni Python; ogni eliminazione verificata contro: catalogo tool DB
(dispatch per nome stringa), uso cross-crate dei `pub`, `next/dynamic`,
pattern framework Python, storia git.

| Fronte | Eliminato | Note |
|---|---|---|
| Cargo.toml | ~40 dipendenze inutilizzate in 13 crate | unico falso positivo: `prost` in mcp-proto (usato dal codice generato), ripristinato con `[package.metadata.cargo-machete] ignored` |
| Rust | 148 item (funzioni/struct/campi/costanti) + cascate | 27 `#[expect(dead_code, reason)]` per i contratti legittimi (auto-scadono se l'item torna in uso) |
| TypeScript | 22 file (tra cui `mcp-connectors.tsx` 28KB sostituito da PluginManager, il pannello knowledge legacy pre-wiki, `environment-panel`) + 81 export | `AdminModal` mantenuto: punto unico in attesa di adozione (wave consolidamento) |
| Python | 33 item + `tool_translator.py` intero (layer dialetti mai cablato) + blocco PR-4 reasoning_bank + `test_classifier_chain.py` (testava una classe rimossa dal commit 627197d) | whitelist vulture creata |

### Bug REALI scoperti dai warning (non erano dead code)

- **`run_tests` scollegato**: tool esposto al modello, raccomandato dai prompt,
  whitelistato (mig 0218/0286), ma senza braccio nel dispatcher — ogni
  invocazione falliva con "Tool run_tests non esiste" (task aperto).
- **Route profili-MCP mai montate**: la pagina `/admin/profiles` chiama
  `GET/PUT /api/admin/profiles/:id/mcp-servers` e
  `GET /api/admin/global-mcp-servers`: gli handler esistevano, le route no
  (né in mcp-core né i proxy Next). Montate + proxy creati nella bonifica.
- **Setting orfana** `nexus_active_routing_pct` (rollout A/B neurale rimosso):
  cleanup in mig 0409.

### Falsi positivi catalogati (NON rimuovere)

- Metodi del checkpointer LangGraph / servicer gRPC / handler FastAPI →
  whitelist vulture.
- Spec Playwright e `playwright.config.ts` → entry in `apps/web-ide/knip.json`.
- `eslint-config-next`, `eslint-plugin-react-hooks`, `@types/cytoscape` →
  usati da eslint/tsc senza import espliciti.
- Varianti di protocollo simmetriche (es. `DetachTab` nel bridge WS) →
  `#[expect]` motivato.
