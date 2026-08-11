// Logica pura del footer costo-per-provider: che cosa si puo' DICHIARARE della
// ripartizione di un run, dato quel che il ledger ha risposto. Estratta dal JSX
// per essere testabile senza React (stesso pattern di use-run-steps-logic.ts).
//
// PERCHE' ESISTE (regole L e Q). Il footer del nastro attivita' mostrava il
// totale del run accanto a un elenco per provider che NON veniva dalla stessa
// fonte: il totale dal ledger, le voci dalle TRACCE del run riprezzate col
// catalogo /api/models. MISURATO il 10/08/2026 su un footer reale:
//
//     346.967 tok | mistral $0.0061 | openrouter qwen3-235b-a22b-2507 $0.0024
//                 | openrouter glm-4.7-flash $0.0000 | openai $0.0000
//                 | tot. $0.0085
//
// Nelle stesse 12 ore il ledger aveva mistral (935 righe), openrouter (119),
// deepseek (94), kimi (15), groq (10), google (3) — e per openai NESSUNA riga.
// Mancavano due provider che avevano chiamato e ne compariva uno che non aveva
// chiamato affatto: non un arrotondamento, due insiemi diversi presentati come
// un elenco e la sua somma.
//
// La regola era gia' scritta per il contatore sotto la chat: ripartizione e
// totale dalla STESSA fonte e collo stesso filtro, o l'elenco non somma al
// totale che gli sta sopra. Qui il perimetro e' quello del RUN
// (`session_usage::Perimetro::RunConDiscendenza`), letto dall'endpoint
// `session-usage` insieme al suo totale.
//
// NIENTE RIPIEGO SULLE TRACCE. Quando la ripartizione non c'e' — backend che non
// parla ancora questo contratto, lettura fallita, run senza righe finalizzate —
// il footer lo DICHIARA. Ricomporla dalle tracce «per non lasciare il posto
// vuoto» rimetterebbe in video esattamente il difetto misurato, con l'aggravante
// di non essere piu' distinguibile dal caso buono.

import type { CurrentRunUsage } from "../../components/chat/token-usage-bar-logic";
import type { RigaRipartizione } from "../api/session-usage-wire";

/**
 * Stato della lettura del perimetro contabile di UN run.
 *
 * L'ignoto e' una VARIANTE e non un oggetto a zeri (regola Q): «non ho ancora
 * letto», «non sono riuscito a leggere» e «ho letto, non ha speso» portano tutti
 * e tre a non mostrare voci, ma dicono cose diverse a chi guarda un contatore di
 * spesa e non possono collassare nello stesso silenzio.
 */
export type RipartizioneRun =
  | { stato: "in_lettura" }
  | { stato: "noto"; perimetro: CurrentRunUsage }
  | { stato: "non_disponibile"; motivo: string };

/** Una voce di ripartizione col provider separato dal modello: e' la forma che
 *  il footer mostra, dove il provider decide colore ed etichetta e il modello
 *  serve solo a distinguere due voci dello stesso provider. */
export interface VoceCostoLedger {
  provider: string;
  model: string;
  tokens: number;
  costUsd: number;
}

/**
 * Le voci di una ripartizione, col provider separato dal modello.
 *
 * L'etichetta la compone il ledger come `provider || '/' || model`
 * (`nexus_ledger::usage_by_model_for_runs`): il provider e' il PRIMO segmento e
 * tutto il resto e' il modello, che a sua volta contiene spesso un `/`
 * (`groq/openai/gpt-oss-20b`, `openrouter/z-ai/glm-4.7-flash`). Tagliare
 * sull'ULTIMO separatore attribuirebbe quelle voci a un provider inesistente, e
 * colore ed etichetta seguirebbero l'attribuzione sbagliata.
 *
 * Un'etichetta senza separatore resta tutta provider, con modello vuoto: e'
 * quel che `etichetteVociCosto` gia' tratta come «niente da distinguere».
 */
export function vociCostoDalLedger(righe: readonly RigaRipartizione[]): VoceCostoLedger[] {
  return righe.map((r) => {
    const taglio = r.model.indexOf("/");
    const separato = taglio > 0;
    return {
      provider: separato ? r.model.slice(0, taglio) : r.model,
      model: separato ? r.model.slice(taglio + 1) : "",
      tokens: r.tokens,
      costUsd: r.costUsd,
    };
  });
}

/**
 * Che cosa il footer ha da mostrare.
 *
 * I quattro modi non sono sfumature dello stesso «niente»: `nessun_consumo` e'
 * una MISURA (il ledger non ha righe finalizzate per questo perimetro, il caso
 * normale di un run appena partito), `solo_totale` e' un totale senza la sua
 * ripartizione (backend anteriore al campo), `non_disponibile` e' l'assenza
 * della lettura. Renderli tutti come «footer che non compare» e' cio' che
 * rendeva invisibile il difetto: un elenco sbagliato e un elenco mancante si
 * assomigliano solo finche' nessuno dei due si dichiara.
 */
export type VistaCostoRun =
  | { modo: "in_lettura" }
  | {
      modo: "voci";
      voci: VoceCostoLedger[];
      totalTokens: number;
      totalCostUsd: number;
      runCount: number;
    }
  | { modo: "solo_totale"; totalTokens: number; totalCostUsd: number; runCount: number }
  | { modo: "nessun_consumo" }
  | { modo: "non_disponibile"; motivo: string };

/**
 * La vista dallo stato di lettura.
 *
 * Il totale mostrato e' quello che il backend DICHIARA per il perimetro, non la
 * somma delle voci: coincidono per costruzione (stessa query, stesso elenco di
 * run), e ricalcolarlo qui creerebbe un secondo produttore dello stesso numero
 * che il giorno di una divergenza mostrerebbe il valore sbagliato senza che
 * nulla lo segnali.
 */
export function vistaCostoRun(r: RipartizioneRun): VistaCostoRun {
  if (r.stato === "in_lettura") return { modo: "in_lettura" };
  if (r.stato === "non_disponibile") return { modo: "non_disponibile", motivo: r.motivo };

  const { totalTokens, totalCostUsd, runCount, breakdown } = r.perimetro;
  const voci = vociCostoDalLedger(breakdown);
  if (voci.length > 0) return { modo: "voci", voci, totalTokens, totalCostUsd, runCount };
  // Nessuna voce: o il ledger non ha ancora righe finalizzate per questo run
  // (allora anche il totale e' zero, ed e' una misura), oppure il totale c'e' e
  // la sua ripartizione no — che e' un contratto a meta', non uno zero.
  if (totalTokens === 0 && totalCostUsd === 0) return { modo: "nessun_consumo" };
  return { modo: "solo_totale", totalTokens, totalCostUsd, runCount };
}

/**
 * Il perimetro gia' in memoria vale per QUESTO run?
 *
 * Il contatore sotto la chat rilegge di continuo il perimetro del run mostrato
 * (`refreshSessionUsage`): per il turno in corso quella e' gia' la risposta
 * giusta, dalla stessa fonte e piu' fresca di una lettura fatta una volta al
 * montaggio. Per un turno STORICO il `runId` non combacia e la risposta va
 * chiesta: e' il criterio che decide se parte una richiesta, e sta qui perche'
 * un `===` sparso nel componente non si prova senza montare React.
 */
export function perimetroGiaNoto(
  nota: CurrentRunUsage | null | undefined,
  runId: string,
): nota is CurrentRunUsage {
  return !!nota && nota.runId === runId;
}
