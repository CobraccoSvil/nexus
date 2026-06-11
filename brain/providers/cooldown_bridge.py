"""Bridge per comunicare errori provider dal brain Python al cooldown Rust (mcp-core).

Quando il brain Python rileva un errore provider (billing_error, rate_limit, ecc.)
in un punto che Rust non osserva direttamente (es. catena classificatore), questo
modulo notifica mcp-core tramite POST /api/internal/provider-error.

mcp-core applica il cooldown appropriato (lungo 6h per billing, breve 60s per
transient) e persiste in Redis — cosi' il routing successivo evita il provider.

Nota: la chiamata e' fire-and-forget (best-effort, timeout 5s).
Non deve mai bloccare il flusso del classificatore.
"""
from __future__ import annotations

import logging
import os

import httpx

logger = logging.getLogger(__name__)

_MCP_CORE_URL: str | None = None


def _get_mcp_core_url() -> str:
    """URL mcp-core: env var (override emergenza) > DB (canonico) > hardcoded."""
    global _MCP_CORE_URL
    if _MCP_CORE_URL is None:
        env = os.environ.get("MCP_CORE_URL")
        if env:
            _MCP_CORE_URL = env.rstrip("/")
        else:
            from brain.utils.settings_db import get_setting
            _MCP_CORE_URL = get_setting("mcp_core_url", "http://127.0.0.1:4000").rstrip("/")
    return _MCP_CORE_URL


async def notify_provider_error(
    provider: str,
    error_class: str | None = None,
    retry_after_seconds: int | None = None,
    *,
    error_text: str | None = None,
    status: int | None = None,
) -> None:
    """Notifica mcp-core Rust di un errore provider per attivare il cooldown.

    Due percorsi (ADR 0032, la classificazione vive nel COMPILATO):
      - classe nota: il chiamante conosce gia' la classe per le proprie
        decisioni locali (es. registry billing) e la passa in ``error_class``;
      - raw (preferito quando la classe servirebbe SOLO al bridge): il
        chiamante passa ``error_text`` (+ ``status``) e mcp-core classifica con
        il punto unico ``provider_error_classifier`` (niente keyword duplicate
        lato Python).

    Args:
        provider: nome del provider (es. "anthropic", "google", "openai")
        error_class: classe errore gia' nota, se il brain la usa anche per se'
        retry_after_seconds: secondi suggeriti dal provider prima di ritentare
        error_text: testo grezzo dell'errore (percorso raw)
        status: HTTP status strutturato, se noto (percorso raw)
    """
    url = f"{_get_mcp_core_url()}/api/internal/provider-error"
    payload: dict[str, object] = {"provider": provider}
    if error_class is not None:
        payload["error_class"] = error_class
    if error_text is not None:
        payload["error_text"] = error_text
    if status is not None:
        payload["status"] = status
    if retry_after_seconds is not None:
        payload["retry_after_seconds"] = retry_after_seconds
    try:
        async with httpx.AsyncClient(timeout=5.0) as client:
            resp = await client.post(url, json=payload)
            if resp.status_code == 200:
                logger.info(
                    "cooldown_bridge: notificato mcp-core provider=%s error_class=%s",
                    provider, error_class,
                )
            else:
                logger.warning(
                    "cooldown_bridge: mcp-core ha risposto %d per provider=%s",
                    resp.status_code, provider,
                )
    except Exception as exc:
        logger.warning(
            "cooldown_bridge: POST %s fallito (provider=%s): %s",
            url, provider, exc,
        )


def notify_provider_error_sync(
    provider: str,
    error_class: str,
    retry_after_seconds: int | None = None,
) -> None:
    """Variante SINCRONA di notify_provider_error per chiamanti non-async.

    Usata da registry._mark_billing_cooldown, che gira dentro
    generate_agent_turn_sync (sync, in un thread executor). Best-effort,
    timeout breve, non solleva mai: serve solo a far persistere il cooldown
    lato mcp-core (Rust = writer unico di nexus_provider_health).
    """
    url = f"{_get_mcp_core_url()}/api/internal/provider-error"
    payload = {
        "provider": provider,
        "error_class": error_class,
    }
    if retry_after_seconds is not None:
        payload["retry_after_seconds"] = retry_after_seconds
    try:
        with httpx.Client(timeout=3.0) as client:
            resp = client.post(url, json=payload)
            if resp.status_code != 200:
                logger.warning(
                    "cooldown_bridge(sync): mcp-core ha risposto %d per provider=%s",
                    resp.status_code, provider,
                )
    except Exception as exc:
        logger.warning(
            "cooldown_bridge(sync): POST %s fallito (provider=%s): %s",
            url, provider, exc,
        )
