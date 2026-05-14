Progetto: app web per noleggio auto a breve termine.

Produci, nell'ordine: (1) spec funzionale e tecnica (PRD con attori, casi d'uso, requisiti non funzionali); (2) scelta dello stack con motivazione scritta nella spec; (3) schema database con migrazioni; (4) backend completo (API + auth + persistenza); (5) frontend (UI prenotazione e admin); (6) suite di test.

Vincoli: nessun modello AI hardcoded nei sorgenti; nessuna emoji in qualsiasi file di codice; nessun comando `docker stop`/`docker compose down`/`docker system prune` su compose di sistema; nessun `unwrap()`/`expect()` fuori da test; nessun log con payload/prompt/response in chiaro. Tutte le modifiche restano dentro la directory del progetto registrato; non toccare file del monorepo IDEAI.

Criterio di accettazione: la verifica automatica dello stack scelto (es. `pnpm verify` per Node, `cargo check + clippy -D warnings + cargo test` per Rust) deve passare sull'intero progetto generato.
