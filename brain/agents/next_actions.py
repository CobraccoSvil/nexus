"""Punto unico per le "scelte di proseguimento" (meta_step next_actions).

Quando una risposta dell'agente propone all'utente delle SCELTE su come
proseguire (es. "Vuoi aggiungere immagini? / Vuoi integrare un form? / Vuoi una
sezione Testimonianze?"), il brain emette un meta_step strutturato che il
frontend rende come pulsanti cliccabili.

Contratto col frontend (concordato, da NON cambiare qui):
    kind    = "next_actions"
    title   = "Prossimi passi"
    payload = {"choices": [{"label": "<testo breve>", "prompt": "<prompt completo>"}, ...]}

Approccio IBRIDO ("entrambi, robusto"):
  1. PRIMARIO — l'agente, istruito dal system prompt (mig 0329), emette alla fine
     della risposta un blocco machine-readable::

         <suggested_actions>
         [{"label":"...","prompt":"..."}, ...]
         </suggested_actions>

     Qui lo estraiamo, lo PARSIAMO e lo RIMUOVIAMO dal testo visibile.
  2. FALLBACK — se l'agente NON ha emesso il blocco ma la risposta propone
     scelte (rilevate con un detector SEMANTICO a embedding, lo stesso
     meccanismo del SemanticRouter — niente regex come unico criterio), un
     modello leggero (purpose 'choices_extractor', mig 0330) estrae le scelte
     in formato {label, prompt}.

Regole rispettate: niente modello hardcoded (G, via purpose_model), nessun leak
di prompt/response in chiaro nei log (F, solo hash/lunghezze), punto unico (L).
"""
from __future__ import annotations

import hashlib
import json
import logging
import re
from typing import Any

from . import meta_steps

logger = logging.getLogger(__name__)

# Contratto frontend.
META_KIND = "next_actions"
META_TITLE = "Prossimi passi"

# Cap difensivi sul payload (label brevi, lista contenuta).
_MAX_CHOICES = 6
_MAX_LABEL_CHARS = 60
_MAX_PROMPT_CHARS = 2000

# Blocco machine-readable emesso dall'agente (primario). Tollerante a spazi,
# maiuscole/minuscole e a eventuali fence markdown attorno al JSON.
_BLOCK_RE = re.compile(
    r"<suggested_actions>\s*(.*?)\s*</suggested_actions>",
    re.IGNORECASE | re.DOTALL,
)

# Euristica leggera per il fallback: la risposta "sembra" proporre scelte se
# contiene piu' domande dirette o formule italiane di proposta/scelta.
_CHOICE_HINT_RE = re.compile(
    r"\b(vuoi|vorresti|preferisci|preferiresti|ti interessa|posso|procediamo con|"
    r"scegli|scegliere|sceglier\w*|scelta|opzion\w*|alternativ\w*|tra cui|"
    r"quale preferisci|come preferisci|fammi sapere)\b",
    re.IGNORECASE,
)

# Elenco (numerato "1." / "1)" o puntato "-", "*", "•") con almeno 2 voci:
# segnale forte di "lista di opzioni" quando accompagnato da un termine di scelta.
_LIST_ITEM_RE = re.compile(r"(?m)^\s*(?:\d+[.)]|[-*•])\s+\S")


def _list_item_count(text: str) -> int:
    return len(_LIST_ITEM_RE.findall(text or ""))


# ── Detector semantico ("la risposta propone scelte all'utente?") ────────────
# Riusa lo STESSO meccanismo embedding+exemplars+cosine del SemanticRouter
# (brain/router/service.py::_classify_by_embedding) e lo STESSO EmbeddingService
# (MiniLM) condiviso del runtime — niente modello aggiuntivo caricato, niente
# duplicazione della logica di interpretazione dei termini (regola L). La
# euristica testuale resta SOLO come rete di sicurezza/fallback.
_CHOICE_EXEMPLARS_POS = [
    "Vuoi che aggiunga immagini reali nella home page?",
    "Preferisci integrare un form di prenotazione direttamente nella home?",
    "Vuoi aggiungere una sezione Testimonianze o Offerte speciali?",
    "Ecco tre possibili migliorie future tra cui puoi scegliere:",
    "Quale di queste opzioni preferisci per proseguire?",
    "Scegli quella che piu' si allinea ai tuoi obiettivi.",
    "Posso procedere in due modi diversi: quale preferisci?",
    "Ti propongo alcune alternative, fammi sapere come vuoi continuare.",
    "Dimmi quale direzione vuoi prendere tra queste.",
]
_CHOICE_EXEMPLARS_NEG = [
    "Ho completato la modifica del file come richiesto.",
    "Il file e' stato salvato correttamente.",
    "Ho corretto l'errore e ora i test passano.",
    "Ecco il riepilogo delle operazioni che ho eseguito.",
    "La configurazione e' terminata con successo.",
    "Ho letto il contenuto del file e procedo con l'analisi.",
]

# Soglie auto-calibranti: 'propone scelte' se il testo e' piu' vicino agli
# exemplar positivi che ai negativi, oltre una similarita' minima assoluta.
_CHOICE_MIN_POS = 0.34
_CHOICE_MARGIN = 0.02

_choice_pos_vecs: list[list[float]] | None = None
_choice_neg_vecs: list[list[float]] | None = None
_choice_ready: bool | None = None  # None=non inizializzato, False=non disponibile


def _shared_embeddings() -> Any:
    """EmbeddingService (MiniLM) condiviso del runtime. Lazy import per evitare
    cicli (runtime importa il grafo agenti). None se non disponibile."""
    try:
        from brain.grpc_server import runtime

        return getattr(runtime, "embeddings", None)
    except Exception:
        return None


def _init_choice_vectors() -> None:
    """Pre-calcola gli embedding degli exemplars (come _init_intent_vectors del
    router). Idempotente; su errore disabilita il detector (fallback testuale)."""
    global _choice_pos_vecs, _choice_neg_vecs, _choice_ready
    if _choice_ready is not None:
        return
    svc = _shared_embeddings()
    if svc is None:
        _choice_ready = False
        return
    try:
        _choice_pos_vecs = [v.values for v in svc.embed_batch("", _CHOICE_EXEMPLARS_POS)]
        _choice_neg_vecs = [v.values for v in svc.embed_batch("", _CHOICE_EXEMPLARS_NEG)]
        _choice_ready = True
        logger.info("next_actions: detector scelte inizializzato (embedding semantici)")
    except Exception as exc:
        logger.warning(
            "next_actions: init detector semantico fallito (%s), uso fallback testuale", exc
        )
        _choice_ready = False


def _top_mean_cosine(query_vec: Any, vecs: list[list[float]], k: int = 3) -> float:
    """Media delle top-k cosine similarity (stesso scoring del SemanticRouter)."""
    import numpy as np

    if not vecs:
        return -1.0
    sims = [
        float(
            np.dot(query_vec, np.array(v))
            / (np.linalg.norm(query_vec) * np.linalg.norm(np.array(v)) + 1e-8)
        )
        for v in vecs
    ]
    sims.sort(reverse=True)
    top = sims[: min(k, len(sims))]
    return sum(top) / len(top)


def _semantic_looks_like_choices(text: str) -> bool | None:
    """Detector a embedding. Ritorna bool se disponibile, None altrimenti
    (cosi' il chiamante ricade sulla rete testuale)."""
    _init_choice_vectors()
    if not _choice_ready or not _choice_pos_vecs:
        return None
    try:
        import numpy as np

        svc = _shared_embeddings()
        qv = np.array(svc.embed_text("", text[:2000]).values)
        pos = _top_mean_cosine(qv, _choice_pos_vecs)
        neg = _top_mean_cosine(qv, _choice_neg_vecs or [])
        return pos >= _CHOICE_MIN_POS and pos > neg + _CHOICE_MARGIN
    except Exception as exc:
        logger.debug("next_actions: detector semantico errore (%s)", exc)
        return None


def _redact(text: str) -> str:
    """Rappresentazione safe-per-log di un testo (regola F): mai il contenuto in
    chiaro, solo lunghezza + hash breve."""
    raw = text or ""
    digest = hashlib.sha1(raw.encode("utf-8", "ignore")).hexdigest()[:10]
    return f"len={len(raw)} sha1={digest}"


def _coerce_choices(raw: Any) -> list[dict[str, str]]:
    """Normalizza una lista grezza di scelte nel formato contrattuale.

    Accetta solo entry con `label` e `prompt` non vuoti (string), tronca i campi
    ai cap difensivi e limita il numero di scelte. Entry malformate scartate.
    """
    if not isinstance(raw, list):
        return []
    out: list[dict[str, str]] = []
    for item in raw:
        if not isinstance(item, dict):
            continue
        label = item.get("label")
        prompt = item.get("prompt")
        if not isinstance(label, str) or not isinstance(prompt, str):
            continue
        label = label.strip()
        prompt = prompt.strip()
        if not label or not prompt:
            continue
        out.append(
            {
                "label": label[:_MAX_LABEL_CHARS],
                "prompt": prompt[:_MAX_PROMPT_CHARS],
            }
        )
        if len(out) >= _MAX_CHOICES:
            break
    return out


def extract_block(text: str) -> tuple[list[dict[str, str]], str]:
    """PRIMARIO: estrae e parsa il blocco <suggested_actions> da `text`.

    Ritorna `(choices, cleaned_text)`:
      - `choices`: lista normalizzata (eventualmente vuota se il blocco manca o
        e' malformato);
      - `cleaned_text`: il testo con OGNI occorrenza del blocco rimossa (l'utente
        non deve mai vedere il blocco grezzo), con whitespace di coda ripulito.

    Non solleva: un blocco malformato => `([], testo-senza-blocco)`.
    """
    if not text or "<suggested_actions>" not in text.lower():
        return [], text or ""

    choices: list[dict[str, str]] = []
    match = _BLOCK_RE.search(text)
    if match:
        inner = match.group(1).strip()
        # Tollera fence markdown attorno al JSON.
        inner = re.sub(r"^```(?:json)?\s*", "", inner)
        inner = re.sub(r"\s*```$", "", inner).strip()
        try:
            parsed = json.loads(inner)
            choices = _coerce_choices(parsed)
        except json.JSONDecodeError:
            logger.warning(
                "next_actions: blocco <suggested_actions> non parsabile (%s)",
                _redact(inner),
            )
            choices = []

    # Rimuove TUTTE le occorrenze del blocco dal testo visibile.
    cleaned = _BLOCK_RE.sub("", text).rstrip()
    return choices, cleaned


def _regex_looks_like_choices(text: str) -> bool:
    """Rete di sicurezza lessicale (no embedding): segnali ovvi di proposta di
    scelte. Usata in OR col detector semantico e come fallback quando gli
    embedding non sono disponibili."""
    if not text:
        return False
    if text.count("?") >= 2:
        return True
    hints = len(_CHOICE_HINT_RE.findall(text))
    if hints >= 2:
        return True
    # Un termine di scelta + un elenco di almeno 2 voci (es. "Ecco 3 opzioni:
    # 1. ... 2. ... 3. ... Scegli quella che...").
    if hints >= 1 and _list_item_count(text) >= 2:
        return True
    return False


def looks_like_choices(text: str) -> bool:
    """Decide se la risposta propone scelte all'utente (gate del fallback LLM).

    PRIMARIO: detector semantico a embedding (stesso meccanismo del
    SemanticRouter, regola L) — interpreta i termini per significato, non per
    keyword. La rete testuale resta come complemento per i segnali lessicali
    ovvi e come fallback quando gli embedding non sono disponibili. Un eventuale
    falso positivo costa al massimo una chiamata a `choices_extractor` che
    ritorna [] (nessun meta_step), quindi si privilegia la copertura.
    """
    if not text:
        return False
    if _semantic_looks_like_choices(text) is True:
        return True
    return _regex_looks_like_choices(text)


def _build_extractor_prompt(assistant_text: str) -> str:
    """Costruisce il prompt per il modello leggero del fallback. Istruisce a
    restituire SOLO JSON (lista) cosi' il parsing e' deterministico."""
    return (
        "Sei un estrattore. Ti viene data la risposta di un assistente AI.\n"
        "Se la risposta propone all'utente delle SCELTE su come proseguire "
        "(opzioni, varianti, prossimi passi suggeriti), estraile.\n\n"
        "Restituisci ESCLUSIVAMENTE un array JSON, senza testo aggiuntivo, nel "
        "formato:\n"
        '[{"label":"<testo breve del pulsante, max 40 caratteri>",'
        '"prompt":"<istruzione completa e non ambigua, pronta da inviare come '
        "messaggio utente per proseguire con quella scelta>\"}]\n\n"
        "Regole per il campo `prompt` (CRITICHE: un prompt mal posto confonde "
        "l'assistente che lo ricevera' e lo costringe a chiedere chiarimenti "
        "invece di agire):\n"
        "- Scrivilo come ISTRUZIONE COMPLETA e NON AMBIGUA in italiano, in seconda "
        "persona verso l'assistente (es. 'Descrivimi...', 'Genera...', 'Modifica...').\n"
        "- Dichiara SEMPRE in modo esplicito l'OUTPUT ATTESO e l'OGGETTO preciso "
        "(quale sezione/elemento/file e con quale obiettivo), cosi' l'assistente "
        "possa eseguire SENZA chiedere chiarimenti.\n"
        "- VIETATE le formule vaghe come 'approfondisci', 'parlami di', 'esplora la "
        "proposta', 'vorrei capire meglio': non dicono cosa produrre. Trasformale "
        "in richieste concrete (es. invece di 'approfondisci la Hero Section' -> "
        "'Descrivimi in dettaglio come rinnovare la Hero Section: struttura, "
        "contenuti, stile e testo della call-to-action').\n"
        "- Se la scelta e' una spiegazione/discussione e NON una modifica al "
        "codice, esplicitalo aggiungendo in coda: 'Per ora forniscimi solo la "
        "proposta dettagliata, senza modificare i file.'\n"
        "- label: conciso, orientato all'azione, in italiano (max 40 caratteri).\n"
        "- Se la risposta NON propone scelte, restituisci esattamente: []\n"
        "- Massimo 6 scelte.\n\n"
        "RISPOSTA DELL'ASSISTENTE:\n"
        "<<<\n"
        f"{assistant_text}\n"
        ">>>"
    )


async def extract_via_llm(assistant_text: str, providers: Any) -> list[dict[str, str]]:
    """FALLBACK: usa il purpose model 'choices_extractor' (mig 0330) per estrarre
    le scelte da una risposta che non conteneva il blocco machine-readable.

    DB-driven (regola G): il modello viene risolto via purpose_model, mai
    hardcoded. Best-effort: qualunque errore (router giu', provider in cooldown,
    JSON malformato) => lista vuota (nessun meta_step), mai eccezione propagata.
    """
    if not assistant_text or providers is None:
        return []
    try:
        from brain.router.service import _routing_client_singleton

        decision = _routing_client_singleton().purpose_model(purpose="choices_extractor")
    except Exception as exc:
        logger.debug("next_actions: purpose 'choices_extractor' non risolto (%s)", exc)
        return []

    provider = decision.provider
    model = decision.model
    if provider in ("__router_unavailable__", "__no_capable_provider__") or model in (
        "__router_unavailable__",
        "__no_capable_provider__",
    ):
        logger.debug(
            "next_actions: fallback saltato, nessun provider per choices_extractor (%s)",
            provider,
        )
        return []

    prompt = _build_extractor_prompt(assistant_text)
    try:
        result = await providers.generate_completion_async(provider, model, prompt)
    except Exception as exc:
        logger.warning("next_actions: estrazione LLM fallita su %s/%s (%s)", provider, model, exc)
        return []

    content = getattr(result, "content", "") or ""
    # Riusa la tolleranza ai fence/oggetti; qui ci aspettiamo una LISTA top-level.
    cleaned = re.sub(r"^```(?:json)?\s*", "", content.strip())
    cleaned = re.sub(r"\s*```$", "", cleaned).strip()
    try:
        parsed = json.loads(cleaned)
    except json.JSONDecodeError:
        logger.debug("next_actions: output extractor non-JSON (%s)", _redact(content))
        return []
    choices = _coerce_choices(parsed)
    if choices:
        logger.info(
            "next_actions: fallback LLM ha estratto %d scelte (modello %s/%s)",
            len(choices), provider, model,
        )
    return choices


def build_step(choices: list[dict[str, str]]) -> dict[str, Any] | None:
    """Costruisce il meta_step next_actions via meta_steps.make (rispetta i flag
    settings). Ritorna None se non ci sono scelte o se il kind e' disabilitato."""
    if not choices:
        return None
    return meta_steps.make(
        kind=META_KIND,
        title=META_TITLE,
        payload={"choices": choices},
    )


async def derive(assistant_text: str, providers: Any) -> tuple[str, dict[str, Any] | None]:
    """Punto unico (regola L) per derivare le scelte di proseguimento.

    Orchestratore dell'approccio ibrido:
      1. tenta il PRIMARIO (blocco <suggested_actions>): se presente, lo usa e
         ripulisce il testo;
      2. altrimenti, se l'euristica suggerisce scelte, tenta il FALLBACK LLM.

    Ritorna `(cleaned_text, meta_step_or_none)`. `cleaned_text` e' SEMPRE il testo
    da mostrare all'utente (privo del blocco grezzo). Il meta_step e' None se non
    sono state trovate scelte o se il kind e' disabilitato dai flag.
    """
    if not assistant_text:
        return assistant_text or "", None

    choices, cleaned = extract_block(assistant_text)
    if choices:
        logger.info("next_actions: %d scelte dal blocco primario", len(choices))
        return cleaned, build_step(choices)

    # Nessun blocco: fallback LLM solo se l'euristica lo giustifica.
    if looks_like_choices(cleaned) and providers is not None:
        llm_choices = await extract_via_llm(cleaned, providers)
        if llm_choices:
            return cleaned, build_step(llm_choices)

    return cleaned, None
