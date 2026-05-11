"""Test sulla classificazione errori provider e propagazione retriable/backoff.

Questi test garantiscono che `error_handler.classify_error()` mappi
correttamente i pattern noti di fallimento provider AI, perche' lo scaling
fra provider in `executor_node` (brain/agents/nodes.py) si basa direttamente
sul valore `stop_reason` e `retriable` ritornati da questa funzione.

Vedi piano sezione 8 (audit token + cooldown billing) e sezione 3 (scaling
provider).
"""
from __future__ import annotations

import pytest

from brain.providers.error_handler import (
    classify_error,
    _extract_provider_message,
    _extract_retry_after,
)


# =========================================================================
# Billing / quota patterns -> cooldown LUNGO (6h lato Rust)
# =========================================================================


@pytest.mark.parametrize(
    "raw_message",
    [
        "Your credit balance is too low to make this request",
        "You exceeded your current quota, please check your plan",
        "Please upgrade or purchase additional credits",
        "Billing required to continue using the API",
        "Account is not active",
        "Payment required: no credits remaining",
        "No credits available on this organization",
        "insufficient_quota",
    ],
)
def test_billing_patterns_classificati_come_billing_error(raw_message: str) -> None:
    """Tutti i pattern di esaurimento credito devono produrre billing_error.

    Questo stop_reason scatena il cooldown lungo (6h) lato Rust in
    brain_agent_client.rs:325, quindi e' la chiave per non riproporre
    un provider fuori credito nello stesso turno.
    """
    info = classify_error(Exception(raw_message), provider="anthropic")
    assert info["stop_reason"] == "billing_error", (
        f"messaggio '{raw_message}' non classificato come billing_error"
    )
    assert info["retriable"] is True, "billing_error deve essere retriable=True per fallback ad altro provider"
    assert info["backoff"] is False, "billing_error NON deve aspettare (passa subito a un altro provider)"


# =========================================================================
# Context too long -> non retriable (cambiare modello non risolve)
# =========================================================================


@pytest.mark.parametrize(
    "raw_message",
    [
        "Request exceeds the maximum context length of this model",
        "Token limit exceeded",
        "Context window of 128000 tokens reached",
        "too many tokens in input",
        "max_tokens has been exceeded for this request",
    ],
)
def test_context_too_long_non_retriable(raw_message: str) -> None:
    info = classify_error(Exception(raw_message), provider="openai")
    assert info["stop_reason"] == "context_too_long"
    assert info["retriable"] is False, (
        "context_too_long NON e' retriable: passare ad altro modello con stesso prompt fallirebbe ugualmente"
    )


# =========================================================================
# Auth patterns -> non retriable
# =========================================================================


@pytest.mark.parametrize(
    "raw_message",
    [
        "invalid_api_key: the provided key has been revoked",
        "API key not valid. Please pass a valid API key",
        "Incorrect API key provided",
        "Authentication failed",
        "Unauthorized access to this endpoint",
        "invalid.api.key",
    ],
)
def test_auth_errors_non_retriable(raw_message: str) -> None:
    info = classify_error(Exception(raw_message), provider="openai")
    assert info["stop_reason"] == "auth_error"
    assert info["retriable"] is False, "auth_error non e' retriable: API key sbagliata su quel provider"


# =========================================================================
# HTTP status codes -> mapping deterministico
# =========================================================================


@pytest.mark.parametrize(
    "status_code,expected_reason,expected_retriable",
    [
        (400, "invalid_request", False),
        (401, "auth_error", False),
        (403, "forbidden", False),
        (404, "not_found", False),
        (413, "context_too_long", False),
        (422, "unprocessable", False),
        (429, "rate_limit", True),
        (500, "provider_error", True),
        (502, "bad_gateway", True),
        (503, "service_unavailable", True),
        (529, "overloaded", True),
    ],
)
def test_http_status_mapping_completo(
    status_code: int, expected_reason: str, expected_retriable: bool
) -> None:
    """Verifica che ogni codice HTTP gestito mappi esattamente al suo stop_reason.

    Questa tabella e' contrattualmente stabile: i call site Rust in
    brain_agent_client.rs::classify_provider_error si basano su questi nomi.
    Modificarli senza coordinamento rompe il fallback automatico.
    """
    raw = f"Error code: {status_code} - request failed"
    info = classify_error(Exception(raw), provider="anthropic")
    assert info["stop_reason"] == expected_reason, (
        f"HTTP {status_code} atteso '{expected_reason}', ottenuto '{info['stop_reason']}'"
    )
    assert info["retriable"] == expected_retriable


# =========================================================================
# Pattern generici (timeout, network, connection)
# =========================================================================


def test_timeout_e_retriable_con_backoff() -> None:
    """Un timeout permette retry dopo backoff (provider potrebbe rispondere)."""
    info = classify_error(Exception("Request timed out after 60s"), provider="mistral")
    assert info["stop_reason"] == "timeout"
    assert info["retriable"] is True
    assert info["backoff"] is True


def test_connection_error_e_retriable_con_backoff() -> None:
    info = classify_error(
        Exception("connection refused: ECONNREFUSED 127.0.0.1:443"),
        provider="deepseek",
    )
    assert info["stop_reason"] == "connection_error"
    assert info["retriable"] is True
    assert info["backoff"] is True


# =========================================================================
# Retry-After parsing
# =========================================================================


def test_retry_after_estratto_da_pattern_testuale() -> None:
    """Quando il provider include 'Retry-After: 120' nel messaggio,
    classify_error deve estrarre l'intero come `retry_after_seconds`."""
    info = classify_error(
        Exception("Error code: 429 - rate limit hit. retry-after: 120"),
        provider="openai",
    )
    assert info["retry_after_seconds"] == 120


def test_retry_after_assente_ritorna_none() -> None:
    info = classify_error(Exception("Some generic error"), provider="")
    assert info["retry_after_seconds"] is None


def test_retry_after_da_response_headers_dict() -> None:
    """Se l'eccezione ha `response.headers`, _extract_retry_after legge
    da li' (priorita' rispetto al pattern testuale)."""

    class FakeHeaders:
        def __init__(self, value: str) -> None:
            self._v = value

        def get(self, key: str, default=None):  # type: ignore[no-untyped-def]
            if key.lower() == "retry-after":
                return self._v
            return default

    class FakeResponse:
        def __init__(self, retry_after: str) -> None:
            self.headers = FakeHeaders(retry_after)

    class FakeAPIError(Exception):
        def __init__(self, msg: str, retry_after: str) -> None:
            super().__init__(msg)
            self.response = FakeResponse(retry_after)

    info = classify_error(
        FakeAPIError("ratelimit", "300"), provider="anthropic"
    )
    assert info["retry_after_seconds"] == 300


# =========================================================================
# Provider message extraction (JSON nested error.message)
# =========================================================================


def test_provider_message_da_json_anthropic() -> None:
    raw = """Error code: 400 - {'error': {'type': 'invalid_request_error', 'message': 'tools.0.input_schema: required'}}"""
    msg = _extract_provider_message(raw)
    assert msg == "tools.0.input_schema: required"


def test_provider_message_da_json_openai() -> None:
    raw = (
        'Error code: 401 - {"error": {"message": "Incorrect API key provided", '
        '"type": "invalid_request_error"}}'
    )
    msg = _extract_provider_message(raw)
    assert msg == "Incorrect API key provided"


def test_provider_message_da_messaggio_piatto() -> None:
    raw = '{"message": "Quota exceeded for this project"}'
    msg = _extract_provider_message(raw)
    assert msg == "Quota exceeded for this project"


def test_provider_message_assente_ritorna_none() -> None:
    raw = "Plain string error without JSON"
    assert _extract_provider_message(raw) is None


# =========================================================================
# Garanzie di scalabilita' provider
# =========================================================================


def test_billing_error_propaga_messaggio_dettagliato_provider() -> None:
    """Quando un provider invia un messaggio diagnostico (es. anthropic con
    'credit balance too low'), classify_error deve preservarlo nel campo
    `message` invece di sostituirlo con il generico — l'utente vede cosi'
    la causa precisa nel banner UI."""
    raw = (
        "Error code: 400 - {'error': {'message': 'Your credit balance is too low "
        "to access the Anthropic API. Please go to Plans & Billing to upgrade.'}}"
    )
    info = classify_error(Exception(raw), provider="anthropic")
    assert info["stop_reason"] == "billing_error"
    # Il messaggio specifico del provider va preservato
    assert "credit balance is too low" in info["message"].lower()


def test_classifier_non_explode_su_eccezione_vuota() -> None:
    """Robustezza: anche un'eccezione senza messaggio non deve crashare la
    chain — il fallback agente continua col prossimo provider."""
    info = classify_error(Exception(""), provider="anthropic")
    assert "stop_reason" in info
    assert "retriable" in info


def test_classifier_distingue_billing_da_rate_limit() -> None:
    """Scenario critico per lo scaling: billing_error e rate_limit hanno
    comportamenti diversi (backoff vs no-backoff). Verifichiamo che non
    si confondano nemmeno se il messaggio HTTP e' 429 ma il body parla di billing."""
    # 429 puro
    rl = classify_error(Exception("Error code: 429 - too many requests"), provider="openai")
    assert rl["stop_reason"] == "rate_limit"

    # billing pattern ha priorita' (controllo testuale prima del codice HTTP)
    bill = classify_error(
        Exception("Error code: 429 - your credit balance is too low"),
        provider="openai",
    )
    assert bill["stop_reason"] == "billing_error"
