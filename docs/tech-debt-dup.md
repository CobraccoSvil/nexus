# Tech debt: duplicazione del codice

Metrica e baseline della duplicazione cross-linguaggio (TS/JS/Rust/Python),
parte dell'operativizzazione della regola L (vedi
[ADR 0026](.nexus-vault/adr/0026-punto-unico-de-duplicazione.md)).

## Come si misura

```bash
bash scripts/dup-report.sh                 # misura + gate ratchet vs baseline
bash scripts/dup-report.sh --report-only   # solo misura
bash scripts/dup-report.sh --update-baseline   # riallinea la baseline (solo al ribasso)
```

Lo strumento e' `jscpd` (config in `jscpd.json`, soglia `minLines=5`, `minTokens=50`).
Il report dettagliato (con i singoli cloni) finisce in `.dup-report/`.

## Gate "ratchet"

Il numero di cloni puo' solo SCENDERE rispetto a `.dup-baseline.json`. Il job CI
in `.github/workflows/verify.yml` fallisce se la duplicazione aumenta. Dopo ogni
wave di consolidamento si riallinea la baseline al ribasso. La baseline non si
alza mai: il debito e' monotono decrescente.

## Baseline

La baseline iniziale e' registrata in `.dup-baseline.json` alla chiusura di Wave 0.
Aggiornare questa tabella a ogni wave che riduce il debito.

| Data | Wave | % righe duplicate | Cloni | Note |
|---|---|---|---|---|
| Wave 0 | 0 | 5.27% (16899 righe) | 1234 | Baseline iniziale, pre-consolidamento (2648 file, 20 formati) |
| Wave 2 | 2 | 5.26% | 1234 | Consolidate TemplateCache/TtlCache + get_template_or_default in nexus-cache/nexus-types. Conteggio jscpd invariato: le due copie differivano nel formato, quindi non erano exact-clone; la duplicazione era strutturale, ora coperta dal guard check-single-source. |
| Wave 3 | 3 | 5.26% | 1233 | get_setting: 4 implementazioni divergenti consolidate in una query unica (read_setting_raw) + viste in nexus-auth; i 3 def-site di mcp-core ora re-export. -1 clone. |
| Wave 4 | 4 | 5.26% | 1233 | Capability gia' fonte unica via ADR 0024 (vista v_model_capabilities): nessun churn. Aggiunto solo guard sul classificatore (classify_capabilities/infer_capabilities_from_name). Cloni invariati. |
| Wave 5 | 5 | 5.25% | 1229 | ensure_required_settings: default statici -> migrazione 0325 (no env var, regola G/H); parte dinamica projects_base_root -> punto unico nexus_types::ensure_projects_base_root (prima duplicata in mcp-core + admin-service). -4 cloni. |

## Hotspot noti (da consolidare)

Riferimento al piano di campagna; i punti unici target sono nel catalogo di ADR 0026.

- Rust: `parse_user_id` (Wave 1), `TemplateCache` (Wave 2), `get_setting` 5 varianti (Wave 3),
  capability detection ~12 file (Wave 4), health/cooldown (Wave 5).
- Python: `psycopg2.connect()` ~31 file (Wave 6a), cache 60s 4 punti (Wave 6b),
  JSON-da-markdown 3+ nodi (Wave 6c), intent duplicati (Wave 6e).
- Frontend: header/modali/stili inline su ~20 pagine admin (Wave 7).
- Cross-language: chunking 4 impl Python vs 1 Rust (Wave 8a).
