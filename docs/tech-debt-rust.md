# Tech debt — Rust

Elenco backlog delle violazioni dogfood nei crate Rust. Popolare con l'output di:

```bash
rg -n '\.unwrap\(\)|\.expect\(' crates/ --glob '!**/tests/**' --glob '!**/*test*.rs'
cargo clippy --workspace --all-targets -- -D warnings 2> clippy.log
```

## Come contribuire

1. Scegliere un crate dalla tabella.
2. Sostituire `unwrap` con `?` + `thiserror`/`anyhow`.
3. Aggiungere test di regressione.
4. Aggiornare lo stato qui sotto.

## Stato attuale (aggiornato dogfood run)

### Produzione — risolti
| File | Riga | Fix applicato |
|---|---|---|
| `crates/admin-service/src/prompt_templates.rs:780` | ex-unwrap | `if let Some(...) else { continue }` |

### Produzione — accettabili (non da correggere)
| File | Riga | Motivazione |
|---|---|---|
| `crates/admin-service/src/main.rs:59` | `.expect("DATABASE_URL")` | Startup fail-fast idiomatico: se manca la var d'ambiente il processo deve terminare |

### Test — unwrap legittimi (non toccare)
Tutti gli `.unwrap()` in `crates/nexus-orchestrator/src/` e simili si trovano dentro
blocchi `#[cfg(test)]` o funzioni di test: uso corretto per assert rapide nei test.

## Hot path prioritari (top 10)

> Compilare con `path:riga — nota`. Tenere aggiornato.

- [ ] `crates/nexus-orchestrator/` — scan iniziale da eseguire
- [ ] `crates/admin-service/` — scan iniziale da eseguire
- [ ] `crates/mcp-core/` — scan iniziale da eseguire
- [ ] `crates/nexus-agents/` — scan iniziale da eseguire
- [ ] `crates/nexus-http/` — scan iniziale da eseguire
- [ ] `crates/nexus-auth/` — sensitivity tier hot path
- [ ] `crates/mcp-db/` — query path
- [ ] `crates/chat-service/`
- [ ] `crates/billing-service/`
- [ ] `crates/doc-service/`

## Clippy warning severi pendenti

- [ ] `unwrap_used` — catalogare dopo prima run con `-W clippy::unwrap_used`.
- [ ] `expect_used` — idem.
- [ ] `panic` su path non-test — idem.

## Doc test aggiunti

- [x] `mcp-core::prompt_templates` — stub doc test
- [x] `nexus-agents::prompt_registry` — stub doc test
- [x] `admin-service::prompt_templates` — stub doc test
