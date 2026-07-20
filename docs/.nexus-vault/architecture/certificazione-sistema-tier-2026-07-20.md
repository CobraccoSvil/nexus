# Certificazione del sistema tier — 2026-07-20

Audit completo eseguito sul sistema VIVO (DB meta :5433, suite 8, HEAD
`d416faeb`), non sui documenti. Ogni affermazione qui sotto e' stata misurata
con una query o un comando, e le anomalie trovate durante l'audit sono state
inseguite fino alla causa prima di firmare.

## Architettura certificata

Il tier di un modello e' una POSIZIONE RELATIVA al migliore del parco, su due
ancore indipendenti, con precedenza `manual > measured > synced > NULL`:

- **Prior (synced)**: `agentic_index` esterno -> bande a % del leader
  (85/65/45/20, ancora persistita con deadband 3%). Mig 0615.
- **Misurato (measured)**: la batteria produce `measured_score` 0-100
  (pesi 12 catena / 45 recupero / 25 stato-latente / 18 real / 0 longctx,
  mig 0620) -> bande a % del leader misurato (92/65/45/20, mig 0617), con
  `min_population=3` e `demote_margin=3`. Score atomico col verdetto; bande
  applicate dal pass di ri-ancoraggio a fine round.
- **Punti unici** (regola L, guard attivi): `tier_from_leader` (la matematica
  delle bande), `apply_tier` + `apply_measured_score` (le uniche scritture),
  `nexus-model-eligibility` (l'eleggibilita', condivisa con `battery-explain`).

## Esiti dell'audit

| Verifica | Esito |
|---|---|
| Pesi score: somma | 100.0 esatto |
| Percentuali bande: ordinate, scale separate | si (prior 0.85 / measured 0.92) |
| Ancora prior vs max indice fresco | 54 = 54.0, combacia |
| Ancora measured vs max score suite 8 | 98.571... = 98.571..., combacia |
| Score fuori range / senza suite | 0 / 0 |
| Rigiocabilita' (seme nelle evidenze multi-step) | 380/380 |
| Test su HEAD | 1323 verdi, 0 rossi |
| Gate quality / single-source | verdi (quality -1: falso positivo in meno) |

## La batteria discrimina (suite 8, ~33 modelli)

La scala di difficolta' e' finalmente una scala:

    agentic_real       98% pass   (gradino base)
    latent_state       84%
    agentic_chain      66%        (ridisegnata: custode + ramo cieco)
    agentic_recovery   26%        (errore parlante; era 0% su tutti)

Il confronto storico che i design precedenti fallivano: sulla catena,
`gemini-3.1-flash-lite` fa 0,0 anelli e `mistral-small` 1,5, mentre i modelli
capaci arrivano a 6,0. Gradiente continuo, non piu' "tutti al tetto".

Bande measured a suite 8: medium 3, high 9, heavy 16, frontier 5 (15%).
Nessuna banda vuota, vertice stretto.

## Anomalia inseguita e chiusa durante l'audit

Tre modelli con score suite-8 alto (80-89) risultavano `medium`: non era un
difetto ma la FINESTRA DICHIARATA dal design — lo score atterra a meta' round,
le bande a fine round. Verificato dal vivo con una sentinella: promossi a
`heavy` dal pass successivo entro 80 secondi. Il sistema si autocorregge come
progettato.

## Caveat dichiarati (una certificazione senza caveat e' un depliant)

1. **381 tier orfani** (`performance_tier` senza `tier_source`), di cui 39
   routabili: fossili pre-discovery, bonifica rimandata deliberatamente quando
   il catalogo era cieco. Ora che l'ingestione e' corretta, la bonifica e'
   possibile: i 39 routabili sono il debito che conta.
2. **`max_models_per_round = 10` e' un valore DA CAMPAGNA** (mig 0623): coperto
   il parco a suite 8 va riportato a 4, o sono ~280 chiamate LLM ogni mezz'ora
   a regime.
3. **Finestra di transizione**: i modelli non ancora rimisurati a suite 8
   (es. `deepseek-v4-pro`, score di suite 6) tengono il tier vecchio per il
   principio "il silenzio non declassa". Si chiude da sola con la copertura.
4. **Anomalia sotto osservazione**: la performance sulla catena NON segue
   l'`agentic_index` (qwen-thinking, indice 3.8, al tetto; grok-4.5, indice
   45.7, a 5.3). O il profilo cattura una capacita' che l'indice non vede, o
   misura qualcosa di laterale: si decide incrociando con recupero e
   stato-latente a parco coperto.
5. **I top recuperatori pubblicati** (anthropic, openai) sono in cooldown
   billing e non ancora misurati sotto la suite nuova: il 26% del recupero e'
   calcolato senza di loro.
6. **`battery-explain` mostra solo le percentuali del prior**: le measured
   (0.92) esistono nei settings ma non compaiono nell'output. Cosmetico, da
   allineare.

## Storia delle tarature (perche' nessuno le ripeta)

- Recupero: `retryable:false` che vietava cio' che misurava (0/30), poi rimedio
  non derivabile (ancora 0), poi errore parlante -> 26% che discrimina.
- Catena: satura a 5 anelli (soffitto dei turni), satura di nuovo a 7 (soffitto
  spostato), poi ridisegno custode+ramo-cieco -> gradiente 0-6.
- Ogni cambio materiale del test ha bumpato la suite (pattern tau2-bench):
  punteggi comparabili solo a parita' di versione.
