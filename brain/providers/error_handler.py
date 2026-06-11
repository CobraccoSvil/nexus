"""
Gestione centralizzata degli errori HTTP dei provider AI.
Mappa ogni codice/tipo di errore in:
  - stop_reason: stringa identificativa per il Rust layer
  - message: messaggio leggibile per l'utente (senza JSON grezzo)
  - retriable: True se ha senso riprovare (backoff), False se è permanente
"""
from __future__ import annotations

import logging
import re
from typing import Any

logger = logging.getLogger(__name__)


# ── Codici HTTP → comportamento ───────────────────────────────────────────────

HTTP_ERROR_MAP: dict[int, dict] = {
    400: {
        "stop_reason": "invalid_request",
        "message": "Richiesta non valida al provider AI.",
        "retriable": False,
    },
    401: {
        "stop_reason": "auth_error",
        "message": "API key non valida o scaduta.",
        "retriable": False,
    },
    403: {
        "stop_reason": "forbidden",
        "message": "Accesso negato dal provider AI (permessi insufficienti).",
        "retriable": False,
    },
    404: {
        "stop_reason": "not_found",
        "message": "Modello non trovato sul provider. Controlla la configurazione.",
        "retriable": False,
    },
    413: {
        "stop_reason": "context_too_long",
        "message": "Contesto troppo lungo per il modello selezionato.",
        "retriable": False,
    },
    422: {
        "stop_reason": "unprocessable",
        "message": "Richiesta non processabile dal provider (parametri errati).",
        "retriable": False,
    },
    429: {
        "stop_reason": "rate_limit",
        "message": "Limite di richieste raggiunto (rate limit). Fallback al provider successivo.",
        "retriable": True,
        "backoff": False,  # quota esaurita → non aspettare, fallback subito
    },
    500: {
        "stop_reason": "provider_error",
        "message": "Errore interno del provider AI. Riprova tra poco.",
        "retriable": True,
        "backoff": True,
    },
    502: {
        "stop_reason": "bad_gateway",
        "message": "Provider AI temporaneamente non raggiungibile (502).",
        "retriable": True,
        "backoff": True,
    },
    503: {
        "stop_reason": "service_unavailable",
        "message": "Provider AI non disponibile (sovraccarico o manutenzione).",
        "retriable": True,
        "backoff": True,
    },
    529: {
        "stop_reason": "overloaded",
        "message": "Provider AI sovraccarico. Fallback al provider successivo.",
        "retriable": True,
        "backoff": True,
    },
}

# ── Pattern testuali per errori billing/quota ─────────────────────────────────

BILLING_PATTERNS = [
    r"credit balance",
    r"insufficient.quota",
    r"upgrade or purchase",
    r"billing",
    r"you exceeded your current quota",
    r"account is not active",
    r"payment required",
    r"no credits",
]

CONTEXT_PATTERNS = [
    r"context.?length",
    r"maximum context",
    r"token.?limit",
    r"too many tokens",
    r"context window",
    r"max_tokens.*exceeded",
]

AUTH_PATTERNS = [
    r"invalid.api.key",
    r"api key not valid",
    r"incorrect api key",
    r"authentication",
    r"unauthorized",
    r"invalid_api_key",
]


def _extract_provider_message(raw: str) -> str | None:
    """Estrae il campo 'message' leggibile da un errore JSON del provider."""
    # Cerca blocco JSON dopo "Error code: NNN - " o simile
    start = raw.find("{")
    if start == -1:
        return None
    json_part = raw[start:]
    # Normalizza single-quote → double-quote (formato Anthropic/OpenAI)
    normalized = json_part.replace("'", '"')
    try:
        import json
        data = json.loads(normalized)
        # Anthropic: {"error": {"message": "..."}}
        # OpenAI:    {"error": {"message": "..."}}
        # Google:    {"error": {"message": "..."}}  o  {"message": "..."}
        msg = (
            data.get("error", {}).get("message")
            or data.get("message")
        )
        if msg and isinstance(msg, str):
            return msg.strip()
    except Exception:
        # Prova con regex come fallback
        m = re.search(r'"message"\s*:\s*"([^"]{10,})"', normalized)
        if m:
            return m.group(1).strip()
    return None


def _extract_retry_after(exc: Exception, raw: str) -> int | None:
    """Estrae il valore dell'header Retry-After dall'eccezione del provider.

    Anthropic/OpenAI/Mistral SDK espongono `response.headers` sull'errore
    `APIStatusError`. Se non disponibile, prova a cercarlo nel testo grezzo.
    Ritorna i secondi (intero) o None se assente.
    """
    # 1. SDK Anthropic/OpenAI/Mistral: exc.response.headers
    try:
        response = getattr(exc, "response", None)
        if response is not None:
            headers = getattr(response, "headers", None)
            if headers:
                val = headers.get("retry-after") or headers.get("Retry-After")
                if val:
                    try:
                        return max(0, int(float(val)))
                    except (ValueError, TypeError):
                        pass
    except Exception:
        pass
    # 2. Pattern testuale di fallback
    m = re.search(r"retry[-_ ]?after[:=\s]+(\d+)", raw, re.IGNORECASE)
    if m:
        try:
            return max(0, int(m.group(1)))
        except ValueError:
            pass
    return None


def _extract_http_status_structured(exc: Exception) -> int | None:
    """HTTP status STRUTTURATO dagli attributi dell'eccezione SDK (contratto
    dati B, censimento 2026-06-10): prima il numero veniva estratto con regex da
    str(exc) ("Error code: 429"), fragile al wording. Gli SDK
    Anthropic/OpenAI/Mistral/DeepSeek espongono `status_code` sull'errore o
    `response.status_code`; httpx espone `response.status_code`. Ritorna None se
    nessun attributo strutturato e' presente (il chiamante ricade sul regex).
    """
    for attr in ("status_code", "status", "code"):
        val = getattr(exc, attr, None)
        if isinstance(val, int) and 100 <= val <= 599:
            return val
    response = getattr(exc, "response", None)
    if response is not None:
        sc = getattr(response, "status_code", None)
        if isinstance(sc, int) and 100 <= sc <= 599:
            return sc
    return None


def classify_error(exc: Exception, provider: str = "") -> dict[str, Any]:
    """
    Classifica un'eccezione del provider e restituisce:
      {
        "stop_reason": str,
        "message": str,         # messaggio leggibile per l'utente
        "retriable": bool,
        "backoff": bool,        # True = aspetta prima di riprovare
        "http_status": int | None,
        "retry_after_seconds": int | None,  # header Retry-After se presente
      }
    """
    raw = str(exc)
    raw_lower = raw.lower()

    # ── 1. Estrai HTTP status code ────────────────────────────────────────────
    # Fonte PRIMARIA: attributo strutturato dell'eccezione SDK (affidabile).
    # Fallback: regex su str(exc) solo se l'SDK non espone lo status (loggato).
    http_status: int | None = _extract_http_status_structured(exc)
    if http_status is None:
        m = re.search(r"(?:Error code|status)[:\s]+(\d{3})", raw, re.IGNORECASE)
        if m:
            http_status = int(m.group(1))
            logger.info("lexical_fallback_used: classify_error/http_status da str(exc)")
    # Estrai Retry-After (prioritario per cooldown dinamico)
    retry_after = _extract_retry_after(exc, raw)

    # ── 2. Pattern billing/quota → fallback immediato ─────────────────────────
    if any(re.search(p, raw_lower) for p in BILLING_PATTERNS):
        provider_msg = _extract_provider_message(raw)
        return {
            "stop_reason": "billing_error",
            "message": provider_msg or "Crediti o quota del provider esauriti. Passaggio al provider successivo.",
            "retriable": True,
            "backoff": False,
            "http_status": http_status,
            "retry_after_seconds": retry_after,
        }

    # ── 3. Context too long ───────────────────────────────────────────────────
    if any(re.search(p, raw_lower) for p in CONTEXT_PATTERNS):
        return {
            "stop_reason": "context_too_long",
            "message": "Contesto troppo lungo per il modello. Prova a ridurre la cronologia o i file allegati.",
            "retriable": False,
            "backoff": False,
            "http_status": http_status,
            "retry_after_seconds": retry_after,
        }

    # ── 4. Auth error ─────────────────────────────────────────────────────────
    if any(re.search(p, raw_lower) for p in AUTH_PATTERNS):
        return {
            "stop_reason": "auth_error",
            "message": f"API key {provider} non valida o scaduta. Verifica la configurazione.",
            "retriable": False,
            "backoff": False,
            "http_status": http_status,
            "retry_after_seconds": retry_after,
        }

    # ── 5. HTTP status code noto ──────────────────────────────────────────────
    if http_status and http_status in HTTP_ERROR_MAP:
        info = HTTP_ERROR_MAP[http_status].copy()
        # Prova a estrarre il messaggio specifico dal JSON
        provider_msg = _extract_provider_message(raw)
        if provider_msg:
            info["message"] = provider_msg
        info["http_status"] = http_status
        if "backoff" not in info:
            info["backoff"] = info.get("retriable", False)
        info["retry_after_seconds"] = retry_after
        return info

    # ── 5bis. Rate-limit testuale ─────────────────────────────────────────────
    # Copre i casi senza status HTTP affidabile: eccezione rilanciata come
    # stringa ("429 Resource has been exhausted", "throttled", ...). Stessa
    # regex del punto unico Rust provider_error_classifier (ADR 0032 / golden
    # tests/fixtures/error_classifier_golden.json). Dopo la HTTP map per design.
    if re.search(r"rate.?limit|too many requests|throttl|\b429\b", raw_lower):
        return {
            "stop_reason": "rate_limit",
            "message": "Rate limit del provider AI raggiunto. Riprovare a breve.",
            "retriable": True,
            "backoff": True,
            "http_status": http_status,
            "retry_after_seconds": retry_after,
        }

    # ── 6. Pattern generici ───────────────────────────────────────────────────
    if "timeout" in raw_lower or "timed out" in raw_lower:
        return {
            "stop_reason": "timeout",
            "message": "Timeout della richiesta al provider AI.",
            "retriable": True,
            "backoff": True,
            "http_status": http_status,
            "retry_after_seconds": retry_after,
        }
    if any(k in raw_lower for k in ("connection", "network", "unreachable", "refused")):
        return {
            "stop_reason": "connection_error",
            "message": f"Provider {provider} non raggiungibile. Verifica la connessione.",
            "retriable": True,
            "backoff": True,
            "http_status": http_status,
            "retry_after_seconds": retry_after,
        }

    # ── 7. Fallback generico: estrai messaggio leggibile ──────────────────────
    provider_msg = _extract_provider_message(raw)
    display = provider_msg or raw[:200]
    logger.warning("Unclassified provider error (%s): %s", provider, raw[:300])
    return {
        "stop_reason": "error",
        "message": display,
        "retriable": False,
        "backoff": False,
        "http_status": http_status,
        "retry_after_seconds": retry_after,
    }


def format_error_result(exc: Exception, provider: str, model: str) -> dict[str, Any]:
    """
    Costruisce il metadata di errore standardizzato da inserire nel ProviderResult.
    Include stop_reason='error' e il messaggio leggibile nel campo 'error'.
    """
    info = classify_error(exc, provider)
    logger.error(
        "Provider error [%s/%s] http=%s stop=%s: %s",
        provider, model,
        info.get("http_status", "?"),
        info["stop_reason"],
        info["message"],
    )
    return {
        "stop_reason": "error",
        "error": info["message"],
        "error_class": info["stop_reason"],
        "retriable": info["retriable"],
        "backoff": info.get("backoff", False),
        "http_status": info.get("http_status"),
        "retry_after_seconds": info.get("retry_after_seconds"),
    }
