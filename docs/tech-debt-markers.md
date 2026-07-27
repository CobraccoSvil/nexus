# Tech debt: marker di debito e frasi di inerzia

Metrica e baseline di due famiglie di marker testuali nel codice sorgente,
parte del "definitivamente" del piano di pulizia dei gap (regola H) e
dell'operativizzazione della regola O (uno strumento di comprensione deve
descrivere il presente, non uno stato passato del porting).

## Perche' esiste

L'audit dei commenti ha trovato, accanto ai buchi funzionali, una categoria a
se': i **commenti fossili**. Un commento scritto durante il porting Python->Rust
dichiara inerte, non cablato o mai raggiunto un percorso che oggi e' in
produzione. E' piu' pericoloso di un TODO: un TODO dice "qui manca qualcosa" ed
e' verificabile; un fossile dice "questo non viene mai eseguito" ed e' **falso**,
inducendo chi legge — umano o agente — a non guardare dove il bug vive davvero.

La wave W8 ha corretto i 35 fossili principali (quelli che dichiaravano morto
codice vivo). Restano molti riferimenti di porting, alcuni tracce d'origine
legittime, altri fossili non ancora ripuliti. Distinguerli testualmente, caso
per caso, e' impossibile senza falsi positivi. Il gate non giudica il singolo
commento: **conta**, e impone che il numero possa solo scendere.

## Come si misura

```bash
bash scripts/markers-ratchet.sh            # misura + gate ratchet vs baseline
bash scripts/markers-ratchet.sh --update   # riallinea la baseline (dopo una pulizia)
```

Due metriche, ognuna col suo tetto, contate come **righe che matchano** (stabile
fra Windows e Linux, indipendente da CRLF) su `crates/` e `apps/` (esclusi
`target`, `target-verify`, `node_modules`, `.next`, `dist`), estensioni `.rs`,
`.ts`, `.tsx`:

- **debt** — marker di debito espliciti: `TODO`, `FIXME`, `HACK`, `XXX`,
  `WORKAROUND`, `DEBITO`.
- **inertia** — dichiarazioni di non-esecuzione, le trappole di lettura:
  `INERTE`, `mai raggiunt*`, `non ancora (cablat|portat|instradat)*`.

Il conteggio del gate e' lo stesso che genera la baseline (`--update`): lo
strumento misura il suo oggetto come lo misura la produzione (regola O).

## Gate "ratchet"

Ogni metrica puo' solo SCENDERE rispetto a `scripts/markers-baseline.json`. Il
gate gira nel pre-commit (`lefthook.yml`, `markers_ratchet`) e in CI
(`.github/workflows/verify.yml`). Fallisce se una metrica sale: si rimuove il
marker introdotto, oppure — se il debito e' giustificato e tracciato altrove —
si riallinea la baseline con `--update`, **dichiarandolo nel commit**. La
baseline non si alza mai senza motivazione: il debito e' monotono decrescente.

Complementare a questo gate: il guard `migrazione-stub` in
`scripts/check-single-source.sh` (introdotto in W8) rifiuta nuove migrazioni il
cui corpo e' solo `SELECT 1;` — informazione distrutta in modo irrecuperabile.

## Baseline

| Data | Wave | debt | inertia | Note |
|---|---|---|---|---|
| 2026-07-23 | W9 | 158 | 32 | Baseline iniziale, dopo la correzione dei 35 fossili principali (W8). |

Aggiornare questa tabella a ogni wave che riduce il debito, sempre al ribasso.
