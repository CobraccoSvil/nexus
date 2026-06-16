"""OpenAI provider adapter.

CHIAMATE LLM: NON eseguite da questo adapter. Dopo il consolidamento del
trasporto (regola L / ADR 0026) tutte le chiamate (generate / agent turn /
stream) passano dal ``GatewayProvider`` (delega al gateway Rust). Questo adapter
resta costruito SOLO per i metodi NON-chiamata ereditati da
``OpenAICompatProviderBase``: ``list_models`` (catalog-sync) e ``test_connection``
(health-check admin), piu' il client SDK on-demand che essi usano. Le quirk
OpenAI delle chiamate (max_completion_tokens per o-series/gpt-5, ruolo
'developer', filtro tool, /v1/responses-only) vivono ora nel gateway Rust
(crates/nexus-gateway/src/providers/openai.rs).
"""
from __future__ import annotations

import logging

from .base import OpenAICompatProviderBase

logger = logging.getLogger(__name__)


class OpenAIProvider(OpenAICompatProviderBase):
    """Provider OpenAI ufficiale.

    Plumbing comune (API key/client, catalogo da DB, guard key mancante,
    test_connection) nel punto unico ``OpenAICompatProviderBase``
    (regola L / ADR 0026, Wave E3).
    """

    name = "openai"
    api_key_label = "OpenAI"
    # client_max_retries=0: i retry sono governati a livello applicativo
    # (cascade nel gateway), non dall'SDK. Mantenuto per il client SDK usato da
    # list_models / test_connection.
    client_max_retries = 0
