# Fixture del confine wire Rust -> TypeScript

Ogni file qui e' **una sola** fixture, prodotta dal produttore Rust reale e
consumata dall'adapter TS reale. Non e' un file scritto a mano che descrive cio'
che il wire dovrebbe essere: e' cio' che il wire e'.

## Perche' una sola, e non una per lato

Il difetto che ha reso necessaria questa cartella e' gia' accaduto: il footer
costo-per-provider mostrava `$0.00` perche' un tipo TS dichiarava
`input_cost_per_million_tokens` mentre il wire Rust, annotato
`#[serde(rename_all = "camelCase")]`, mandava `inputCostPerMillionTokens`. Ogni
lettura era `undefined` e un `?? 0` a valle la trasformava in un numero
plausibile. I test dei due lati erano verdi: ciascuno misurava la propria idea
del contratto, e nessuno la giunzione (regola O).

Con una fixture sola questo non e' piu' rappresentabile:

- il test **Rust** asserisce che la funzione che COSTRUISCE la risposta produce
  esattamente questo JSON (`include_str!`, quindi il percorso non dipende dalla
  directory di lavoro di chi lancia il test);
- il test **TS** dà questo stesso JSON in pasto alla funzione dell'adapter che
  la produzione usa davvero, con `fetch` sostituito.

Rinominare un campo da un lato fa rosseggiare il test di quel lato; aggiornare la
fixture per farlo tornare verde fa rosseggiare l'altro. Il contratto non si puo'
cambiare a meta'.

## Regole

- **Non modificare a mano** un file di questa cartella per far passare un test.
  Se il wire e' cambiato per davvero, si aggiornano insieme fixture, produttore e
  consumatore, nello stesso commit.
- I valori sono realistici e, dove possibile, MISURATI: servono anche a
  documentare l'ordine di grandezza atteso.

## Inventario

| file | produttore | consumatore |
|---|---|---|
| `session-usage.json` | `crates/mcp-core/src/billing.rs` (`corpo_session_usage`) | `lib/api/billing.ts` (`getSessionUsage`) |
| `gateway-providers.json` | `crates/mcp-core/src/environment.rs` (`entry_con_prontezza`) | `lib/api/gateway-providers.ts` (`renderReadiness`, `renderDeclaration`) |

### gateway-providers.json

Catturata dall'endpoint reale il 10/08/2026. A differenza di `session-usage.json`
**non e' ancorata da un test Rust**: il produttore compone l'entry da piu' punti
(`entry_con_prontezza` piu' i rami che aggiungono health e cooldown) e non esiste
una funzione sola che produca l'intero corpo. Dichiararlo qui e' meglio che
lasciarlo intuire — se un giorno quel corpo avra' un compositore unico, questa
fixture va ancorata con `include_str!` come l'altra.

I campi `declaration` / `declaration_undeclared` portano la misura del
10/08/2026 su `ai_price_catalog` incrociato con `v_model_capabilities`: 37
modelli abilitati su 128 non hanno una riga di capability (openrouter 17 su 17,
openai 11 su 65, perplexity 3 su 3, groq 2 su 2, anthropic 2 su 9, google 2 su
9). `groq` e `openrouter` sono nella fixture con `readiness: "healthy"` E
`declaration: "absent"`: e' la coppia di valori che rende il caso non
rappresentabile con un campo solo.

### session-usage.json

I numeri vengono dalla misura dell'08/08/2026 sulla sessione `ec643216` del
progetto gestione-corsi (758 righe `finalized` in `ai_usage_ledger`): la stessa
sessione su cui il contatore dichiarava `639 token - $2.14`. Il costo e'
arrotondato a quattro decimali come lo mostra la UI.

Il campo `current_run` e' `null` quando la richiesta non chiede un run o quando
il run non appartiene alla sessione: e' un'assenza, non un oggetto a zeri —
«non ho un perimetro di run» e «questo run non e' costato nulla» sono due cose
diverse (regola Q).
