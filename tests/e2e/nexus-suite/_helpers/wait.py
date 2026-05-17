"""Polling agent_runs fino a status terminale."""
import time
from .db import fetchone
from .cfg import cfg


TERMINAL = {"completed", "failed", "timed_out", "cancelled", "loop_aborted", "provider_unavailable"}


def wait_for_run(run_id: str, timeout_s: int | None = None, poll_s: float = 3.0) -> dict:
    """Polla agent_runs.status fino a terminale o timeout.

    Ritorna l'ultimo row letto. Solleva TimeoutError se non terminato.
    """
    deadline = time.time() + (timeout_s or cfg.scenario_timeout_s)
    last = None
    while time.time() < deadline:
        last = fetchone(
            "SELECT id::text, status, iteration_count, provider, model, final_answer, "
            "       EXTRACT(EPOCH FROM (COALESCE(completed_at,NOW())-created_at))::int AS dur_s "
            "FROM agent_runs WHERE id = %s",
            (run_id,),
        )
        if last and last["status"] in TERMINAL:
            return dict(last)
        time.sleep(poll_s)
    raise TimeoutError(f"run {run_id} non terminato in {timeout_s or cfg.scenario_timeout_s}s (ultimo: {last})")
