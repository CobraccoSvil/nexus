"""Rolling conversation summarization (BP4 piano riduzione token).

Sostituisce N messaggi vecchi (>last_n_intact) con un singolo messaggio
"assistant" contenente un riassunto compresso. Viene attivato quando il
contesto stimato supera una soglia configurabile (default: 60% di
MAX_CONTEXT_CHARS) per evitare di colpire il limite del modello.

Pattern di chiamata sub-LLM riusato da reflection_node (brain/agents/nodes.py).
Il modello usato e' un "small/fast" (Haiku/Mini) configurato in
nexus_purpose_model con purpose='conversation_summary' e fallback a Haiku.

Persistenza opzionale: se il DB pool e' disponibile, scrive una riga in
nexus_conversation_summaries per audit/replay (tabella creata in
db/migrations/0117_conversation_summaries.sql).
"""
from __future__ import annotations

import asyncio
import json
import logging
import time
from typing import Any

logger = logging.getLogger(__name__)


class SummaryModelUnavailable(Exception):
    """Sollevata quando il modello per il summarizer non puo' essere risolto
    da `nexus_purpose_model` (DB irraggiungibile o purpose `conversation_summary`
    non configurato).

    Regola G di CLAUDE.md: vietato silently-fallback a un modello hardcoded.
    Il caller (`summarize_if_needed`) gestisce l'eccezione skippando il
    summarizer per quel turno — la conversazione continua senza compressione.
    """
    pass


# Soglia: avvia il summarizer quando il contesto stimato supera questa frazione
# di MAX_CONTEXT_CHARS (importato da nodes.py). Configurabile via env per test.
SUMMARY_TRIGGER_FRACTION = 0.60

# Numero di messaggi recenti da preservare integri (no compressione).
# Anthropic prompt caching breakpoint si appoggia su questi messaggi.
DEFAULT_KEEP_RECENT = 6

# Limite output del riassunto: 800 token ~ 3200 char circa.
SUMMARY_MAX_TOKENS = 800
SUMMARY_TIMEOUT_S = 15.0
SUMMARY_TEMPERATURE = 0.0

# Prompt template per il summarizer. Tag XML chiari per separare istruzioni
# (best practice Anthropic). Italiano coerente con il resto del codebase.
_SUMMARIZE_SYSTEM = """Sei un assistente che produce riassunti tecnici concisi di conversazioni multi-turno fra utente e agente AI sviluppatore.

<obiettivo>
Comprimere la cronologia in un singolo riassunto strutturato che preservi le informazioni operative critiche e permetta all'agente di continuare il lavoro senza perdere contesto.
</obiettivo>

<contenuto_obbligatorio>
- File letti / modificati (path completi)
- Errori incontrati e fix applicati
- Decisioni prese (architetturali, di scelta libreria, di refactor)
- Comandi eseguiti con esito
- Stato attuale del task: cosa e' fatto, cosa resta
</contenuto_obbligatorio>

<formato_output>
Markdown strutturato con sezioni: ## File toccati, ## Errori e fix, ## Decisioni, ## Stato. Niente preamboli ne' chiusure conversazionali. Massimo 800 token.
</formato_output>"""


def _serialize_message(msg: Any) -> str:
    """Serializza un messaggio in formato leggibile per il summarizer."""
    role = getattr(msg, "type", None) or getattr(msg, "role", "?")
    if hasattr(msg, "content"):
        content = msg.content
    elif isinstance(msg, dict):
        role = msg.get("role", role)
        content = msg.get("content", "")
    else:
        content = str(msg)
    if isinstance(content, list):
        # Anthropic-style content blocks: estrai solo il testo
        parts: list[str] = []
        for block in content:
            if isinstance(block, dict):
                btype = block.get("type")
                if btype == "text":
                    parts.append(block.get("text", ""))
                elif btype == "tool_use":
                    name = block.get("name", "?")
                    inp = block.get("input", {})
                    parts.append(f"[tool_use {name}({json.dumps(inp, ensure_ascii=False)[:200]})]")
                elif btype == "tool_result":
                    res = block.get("content", "")
                    if isinstance(res, list):
                        res = " ".join(str(b.get("text", ""))[:200] for b in res if isinstance(b, dict))
                    parts.append(f"[tool_result {str(res)[:300]}]")
        content = "\n".join(parts)
    return f"[{role}]\n{content}"


async def summarize_old_messages(
    messages: list[Any],
    *,
    providers: Any,
    keep_recent: int = DEFAULT_KEEP_RECENT,
    model: str | None = None,
    db_pool: Any = None,
    thread_id: str | None = None,
) -> list[Any] | None:
    """Comprimi i messaggi vecchi in un singolo riassunto.

    Args:
        messages: lista completa dei messaggi.
        providers: ProviderRegistry (necessario per chiamare il modello small).
        keep_recent: quanti messaggi finali preservare integri.
        model: override del modello. Se None, usa Haiku come default.
        db_pool: asyncpg pool opzionale per persistenza audit.
        thread_id: id thread per audit DB.

    Returns:
        Nuova lista [summary_msg, ...messages[-keep_recent:]] oppure None
        in caso di errore (caller deve continuare con la lista originale).
    """
    if providers is None:
        logger.debug("summarizer: providers None, skip")
        return None
    if len(messages) <= keep_recent + 1:
        # Niente da comprimere
        return None

    old = messages[: len(messages) - keep_recent]
    recent = messages[len(messages) - keep_recent :]

    if not old:
        return None

    # Prepara il prompt utente: serializza i messaggi vecchi
    serialized_parts: list[str] = []
    for m in old:
        s = _serialize_message(m)
        if s.strip():
            serialized_parts.append(s)
    user_prompt = (
        "<conversazione_da_riassumere>\n"
        + "\n---\n".join(serialized_parts)
        + "\n</conversazione_da_riassumere>\n\n"
        "Produci ora il riassunto seguendo il formato richiesto."
    )

    # Selezione provider: preferisce Anthropic (Haiku), fallback su OpenAI o altri.
    # Nota: `providers` e' ProviderRegistry — usare il registry direttamente per
    # evitare di chiamare metodi sul singolo provider che non li espone.
    use_provider = "anthropic" if providers._providers.get("anthropic") else None
    if use_provider is None:
        for name in ("openai", "deepseek", "google"):
            if providers._providers.get(name):
                use_provider = name
                break
    if use_provider is None:
        logger.warning("summarizer: nessun provider disponibile")
        return None
    # Verifica che il registry esponga generate_completion_async (e' un metodo del registry,
    # NON dei singoli provider — controlla prima di chiamare per evitare AttributeError silente).
    if not hasattr(providers, "generate_completion_async"):
        logger.warning("summarizer: ProviderRegistry senza generate_completion_async, skip")
        return None

    if model:
        use_model = model
    else:
        try:
            use_model = _resolve_summary_model(use_provider)
        except SummaryModelUnavailable as exc:
            # Regola G: niente fallback hardcoded. Se il modello non e'
            # configurato in DB, saltiamo il summarizer per questo turno —
            # la conversazione continuera' senza compressione finche'
            # l'admin non popola `nexus_purpose_model` (purpose='conversation_summary').
            logger.warning(
                "summarizer: modello non risolvibile da DB (%s), skip summarizer per questo turno",
                exc,
            )
            return None

    full_prompt = f"{_SUMMARIZE_SYSTEM}\n\n{user_prompt}"
    # Clamp difensivo (punto unico, regola L): il summarizer riceve l'intera
    # history serializzata come user_prompt -> il prompt full puo' superare il
    # window del modello scelto. Cap a max_context_ratio * window con head+tail.
    from brain.agents.context_brake import clamp_single_prompt

    full_prompt = clamp_single_prompt(full_prompt, use_model)
    t0 = time.monotonic()
    try:
        result = await asyncio.wait_for(
            providers.generate_completion_async(
                use_provider,
                use_model,
                full_prompt,
                max_tokens=SUMMARY_MAX_TOKENS,
                temperature=SUMMARY_TEMPERATURE,
                internal_task=True,
            ),
            timeout=SUMMARY_TIMEOUT_S,
        )
        summary_text = result.content if hasattr(result, "content") else str(result)
    except asyncio.TimeoutError:
        logger.warning("summarizer: timeout dopo %.1fs", SUMMARY_TIMEOUT_S)
        return None
    except Exception as exc:
        logger.error("summarizer: errore chiamata LLM: %s", exc)
        return None

    elapsed_ms = int((time.monotonic() - t0) * 1000)
    if not summary_text or len(summary_text.strip()) < 50:
        logger.warning("summarizer: riassunto troppo corto/vuoto, abbandono")
        return None

    logger.info(
        "summarizer: compresso %d messaggi -> riassunto (%d char) in %dms con %s/%s",
        len(old), len(summary_text), elapsed_ms, use_provider, use_model,
    )

    # Persistenza audit (best-effort, non blocca)
    if db_pool is not None and thread_id:
        try:
            await _persist_summary_audit(
                db_pool, thread_id, len(old), summary_text, use_model, elapsed_ms,
            )
        except Exception as exc:
            logger.debug("summarizer: persist audit fallito: %s", exc)

    # Costruisce il messaggio riassunto in formato compatibile con LangGraph.
    # Importazione lazy per evitare dipendenze circolari.
    try:
        from langchain_core.messages import AIMessage  # type: ignore[import-untyped]
        summary_msg = AIMessage(content=(
            "[Riassunto automatico della conversazione precedente]\n\n" + summary_text.strip()
        ))
    except ImportError:
        # Fallback: dict in formato Anthropic
        summary_msg = {
            "role": "assistant",
            "content": (
                "[Riassunto automatico della conversazione precedente]\n\n"
                + summary_text.strip()
            ),
        }

    return [summary_msg, *recent]


def _resolve_summary_model(provider: str) -> str:
    """Risolve il modello da usare per il summarizer leggendo
    `nexus_purpose_model` (purpose='conversation_summary').

    Comportamento (regola G di CLAUDE.md):
    - DB OK e purpose configurato: ritorna `model_id` letto dal DB
    - DB irraggiungibile o purpose mancante: solleva `SummaryModelUnavailable`

    Niente fallback hardcoded: se il modello non e' configurato il caller
    (`summarize_if_needed`) deve degradare correttamente (skip summarizer)
    invece di scegliere un modello a caso.

    Il parametro `provider` e' usato solo per logging diagnostico (l'admin
    sceglie provider+model insieme tramite `nexus_purpose_model`).
    """
    import os
    database_url = os.environ.get("DATABASE_URL")
    if not database_url:
        raise SummaryModelUnavailable(
            "DATABASE_URL non impostata: impossibile leggere "
            "nexus_purpose_model. Configurare la variabile d'ambiente."
        )
    try:
        from brain.utils.db_pool import connect as _db_connect
        with _db_connect() as conn, conn.cursor() as cur:
            cur.execute(
                "SELECT provider, model_id FROM nexus_purpose_model "
                "WHERE purpose = 'conversation_summary'",
            )
            row = cur.fetchone()
    except Exception as exc:
        raise SummaryModelUnavailable(
            f"DB irraggiungibile: {exc}. Verifica Postgres e migrazione 0102."
        ) from exc

    if not row:
        raise SummaryModelUnavailable(
            "purpose 'conversation_summary' non configurato in nexus_purpose_model. "
            "Applicare la migrazione 0102 e popolare la tabella."
        )
    db_provider, db_model = row
    if db_provider != provider:
        logger.debug(
            "summarizer: provider corrente '%s' diverso da quello in purpose_model '%s', "
            "uso comunque il modello dal DB",
            provider, db_provider,
        )
    logger.debug(
        "summarizer: modello da DB purpose_model: %s/%s", db_provider, db_model
    )
    return db_model


async def _persist_summary_audit(
    db_pool: Any,
    thread_id: str,
    replaced_count: int,
    summary_text: str,
    model_used: str,
    elapsed_ms: int,
) -> None:
    """Inserisce riga di audit in nexus_conversation_summaries."""
    async with db_pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO nexus_conversation_summaries
              (thread_id, replaced_msg_count, summary_text, model_used, latency_ms)
            VALUES ($1, $2, $3, $4, $5)
            """,
            thread_id, replaced_count, summary_text, model_used, elapsed_ms,
        )


def should_trigger_summary(context_chars: int, max_context_chars: int) -> bool:
    """Decisione idempotente: il caller ha gia' un context_chars stimato."""
    return context_chars >= int(max_context_chars * SUMMARY_TRIGGER_FRACTION)
