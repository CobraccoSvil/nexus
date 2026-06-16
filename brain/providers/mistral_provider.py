"""Mistral provider adapter (OpenAI-compatible API).

CHIAMATE LLM: NON eseguite da questo adapter. Dopo il consolidamento del
trasporto (regola L / ADR 0026) tutte le chiamate (generate / agent turn)
passano dal ``GatewayProvider`` (delega al gateway Rust). Questo adapter resta
costruito SOLO per i metodi NON-chiamata ereditati da
``OpenAICompatProviderBase``: ``list_models`` (catalog-sync) e ``test_connection``
(health-check admin), piu' il client SDK on-demand che essi usano. Le quirk
Mistral delle chiamate (prompt_cache_key esplicito, weak-models su tool_choice,
truncation finish_reason=length, trailing-assistant strip) vivono ora nel gateway
Rust (crates/nexus-gateway/src/providers/).
"""
from __future__ import annotations

import logging

from .base import OpenAICompatProviderBase

logger = logging.getLogger(__name__)

BASE_URL = "https://api.mistral.ai/v1"


class MistralProvider(OpenAICompatProviderBase):
    """Provider Mistral (OpenAI-compatible endpoint).

    Plumbing comune (API key/client, catalogo da DB, guard key mancante,
    test_connection) nel punto unico ``OpenAICompatProviderBase``
    (regola L / ADR 0026, Wave E3).
    """

    name = "mistral"
    base_url = BASE_URL
    api_key_label = "Mistral"
