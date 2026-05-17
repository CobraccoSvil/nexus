"""Bridge tra reflection_node e reasoning_bank (Fase 2).

Quando il punteggio di reflection raggiunge la soglia di qualita' (default 0.85),
questo modulo persiste l'esempio di successo nelle tabelle reasoning_patterns /
reasoning_examples per l'arricchimento futuro dei few-shot prompts.

La logica e' fire-and-forget: eventuali errori DB vengono loggati ma non
propagati al chiamante, per non bloccare il grafo agente.

Schema dipendente:
    - reasoning_patterns (UUID, pattern_type, name, description, source_agent, ...)
    - reasoning_examples (UUID, pattern_id, input_summary, output_summary, context, quality_score)

Definiti in: db/migrations/0055_reasoning_bank.sql
"""
from __future__ import annotations

import logging
from typing import Any

logger = logging.getLogger(__name__)

# Stringa template per il nome del pattern legato a un prompt_key
_PATTERN_NAME_TPL = "Successo agente: {prompt_key}"


def _get_db_pool() -> Any | None:
    """Ottiene il pool PostgreSQL condiviso da nexus-neural.

    Usa il modulo brain.db se disponibile, altrimenti None.
    Import lazy per evitare circolarita' e permettere il test senza DB.
    """
    try:
        from brain import db  # type: ignore[import]
        return db.get_pool()
    except Exception:
        return None


async def maybe_store_reflection_example(
    prompt_key: str,
    prompt_version: int,
    task_input: str,
    agent_output: str,
    reflection: dict[str, Any],
    profile_name: str | None = None,
    lang: str | None = None,
) -> bool:
    """Persiste un esempio di successo nel reasoning bank se score >= soglia.

    Args:
        prompt_key: Chiave del prompt agente (es. "agent.coder.base").
        prompt_version: Versione del prompt usato nel run.
        task_input: Testo del task originale (HumanMessage).
        agent_output: Output prodotto dall'agente (result).
        reflection: Dict restituito da parse_reflection_response().
        profile_name: Nome del profilo agente (es. "coder").
        lang: Linguaggio dominante del repository (opzionale).

    Returns:
        True se l'esempio e' stato inserito, False altrimenti.
    """
    score = float(reflection.get("score", 0.0))
    suggestions = reflection.get("suggestions") or []

    # La soglia di score viene applicata dal chiamante (reflection_node in nodes.py),
    # che legge reflection_reasoning_bank_min_score dal DB via reflection_config.
    # Qui verifichiamo solo che i suggestions siano presenti.
    if not suggestions:
        logger.debug(
            "reasoning_bank: nessun suggestion per '%s' (score=%.3f), skip insert",
            prompt_key, score,
        )
        return False

    pool = _get_db_pool()
    if pool is None:
        logger.warning("reasoning_bank: pool DB non disponibile, skip insert")
        return False

    try:
        async with pool.acquire() as conn:
            # Cerca o crea il pattern per questo prompt_key
            pattern_id = await _upsert_pattern(conn, prompt_key, profile_name)
            if pattern_id is None:
                return False

            # Inserisce l'esempio
            await _insert_example(
                conn,
                pattern_id=pattern_id,
                task_input=task_input,
                agent_output=agent_output,
                reflection=reflection,
                prompt_key=prompt_key,
                prompt_version=prompt_version,
                lang=lang,
            )

            # Aggiorna contatori success del pattern
            await conn.execute(
                """
                UPDATE reasoning_patterns
                SET use_count    = use_count + 1,
                    success_count = success_count + 1,
                    confidence   = LEAST(1.0, confidence + 0.01),
                    updated_at   = NOW()
                WHERE id = $1
                """,
                pattern_id,
            )

        logger.info(
            "reasoning_bank: inserito esempio per '%s' v%d score=%.3f",
            prompt_key, prompt_version, score,
        )
        return True

    except Exception as exc:
        logger.error(
            "reasoning_bank: errore durante insert per '%s': %s",
            prompt_key, exc,
        )
        return False


async def _upsert_pattern(conn: Any, prompt_key: str, profile_name: str | None) -> Any | None:
    """Cerca il pattern per questo prompt_key o ne crea uno nuovo.

    Restituisce l'UUID del pattern oppure None in caso di errore.
    """
    nome = _PATTERN_NAME_TPL.format(prompt_key=prompt_key)

    # Cerca pattern esistente per questo prompt_key (nel campo tags)
    row = await conn.fetchrow(
        """
        SELECT id FROM reasoning_patterns
        WHERE $1 = ANY(tags)
        LIMIT 1
        """,
        f"prompt_key:{prompt_key}",
    )
    if row is not None:
        return row["id"]

    # Crea pattern nuovo
    try:
        row = await conn.fetchrow(
            """
            INSERT INTO reasoning_patterns
                (pattern_type, name, description, source_agent, tags,
                 applicable_tasks, confidence)
            VALUES
                ('problem_solving', $1, $2, $3, ARRAY[$4::text], ARRAY[$5::text], 0.5)
            RETURNING id
            """,
            nome,
            f"Pattern di successo per l'agente {profile_name or prompt_key}. "
            f"Prompt key: {prompt_key}.",
            profile_name or "nexus-agent",
            f"prompt_key:{prompt_key}",
            prompt_key,
        )
        return row["id"] if row else None
    except Exception as exc:
        logger.error("reasoning_bank: upsert_pattern fallito: %s", exc)
        return None


async def _insert_example(
    conn: Any,
    pattern_id: Any,
    task_input: str,
    agent_output: str,
    reflection: dict[str, Any],
    prompt_key: str,
    prompt_version: int,
    lang: str | None,
) -> None:
    """Inserisce un record in reasoning_examples."""
    import json

    context = {
        "prompt_key": prompt_key,
        "prompt_version": prompt_version,
        "lang": lang,
        "dimensions": reflection.get("dimensions"),
        "suggestions": reflection.get("suggestions"),
    }
    await conn.execute(
        """
        INSERT INTO reasoning_examples
            (pattern_id, input_summary, output_summary, context, quality_score, validated)
        VALUES ($1, $2, $3, $4::jsonb, $5, FALSE)
        """,
        pattern_id,
        task_input[:500] if task_input else "",
        agent_output[:500] if agent_output else "",
        json.dumps(context),
        float(reflection.get("score", 0.0)),
    )




# PR-4: planner few-shot integration.
# Persiste plan completati con successo (verifier passed + final_reward >= soglia)
# come esempi few-shot per planner_node nelle invocazioni future con intent
# scaffold_app / architecture.

_PLAN_PATTERN_TYPE = "plan_template"
_PLAN_REWARD_THRESHOLD = 0.85


async def maybe_store_successful_plan(
    run_id: str,
    intent: str,
    user_task: str,
    plan_summary: str,
    final_reward: float,
) -> bool:
    """Persiste un plan vincente come esempio pattern_type=plan_template.

    Soglia configurabile via setting orchestrator.reasoning_bank_plan_min_score
    (fallback 0.85). Filtra per intent in {scaffold_app, architecture, code, implement}.
    """
    if intent not in ("scaffold_app", "architecture", "code", "implement"):
        return False
    pool = _get_db_pool()
    if pool is None:
        return False
    try:
        async with pool.acquire() as conn:  # type: ignore[union-attr]
            threshold_row = await conn.fetchrow(
                "SELECT value FROM settings WHERE key='orchestrator.reasoning_bank_plan_min_score' LIMIT 1"
            )
            threshold = _PLAN_REWARD_THRESHOLD
            if threshold_row and threshold_row["value"]:
                try:
                    threshold = float(threshold_row["value"])
                except Exception:
                    pass
            if final_reward < threshold:
                return False
            pattern_id = await conn.fetchval(
                """INSERT INTO reasoning_patterns
                   (pattern_type, name, description, source_agent)
                   VALUES ($1, $2, $3, $4)
                   ON CONFLICT DO NOTHING
                   RETURNING id""",
                _PLAN_PATTERN_TYPE,
                f"plan:{intent}",
                f"Plan riuscito per intent={intent}",
                "planner_node",
            )
            if pattern_id is None:
                pattern_id = await conn.fetchval(
                    "SELECT id FROM reasoning_patterns WHERE pattern_type=$1 AND name=$2 LIMIT 1",
                    _PLAN_PATTERN_TYPE, f"plan:{intent}",
                )
            import json as _json
            await conn.execute(
                """INSERT INTO reasoning_examples
                   (pattern_id, input_summary, output_summary, context, quality_score, validated)
                   VALUES ($1, $2, $3, $4::jsonb, $5, FALSE)""",
                pattern_id,
                (user_task or "")[:500],
                (plan_summary or "")[:1500],
                _json.dumps({"run_id": run_id, "intent": intent}),
                float(final_reward),
            )
        logger.info(
            "reasoning_bank: plan run_id=%s intent=%s reward=%.3f salvato come few-shot",
            run_id, intent, final_reward,
        )
        return True
    except Exception as exc:
        logger.debug("maybe_store_successful_plan fallita: %s", exc)
        return False


async def retrieve_similar_plans(
    user_task: str,
    intent: str | None = None,
    limit: int = 3,
) -> list[dict[str, Any]]:
    """Ritorna fino a `limit` plan riusciti per few-shot (ordinati per score)."""
    pool = _get_db_pool()
    if pool is None:
        return []
    try:
        async with pool.acquire() as conn:  # type: ignore[union-attr]
            rows = await conn.fetch(
                """SELECT e.input_summary, e.output_summary, e.quality_score
                   FROM reasoning_examples e
                   JOIN reasoning_patterns p ON p.id = e.pattern_id
                   WHERE p.pattern_type = $1
                     AND ($2::text IS NULL OR p.name = 'plan:' || $2)
                   ORDER BY e.quality_score DESC, e.created_at DESC
                   LIMIT $3""",
                _PLAN_PATTERN_TYPE,
                intent,
                int(limit),
            )
        return [dict(r) for r in rows]
    except Exception as exc:
        logger.debug("retrieve_similar_plans fallita: %s", exc)
        return []
