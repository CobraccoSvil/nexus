"""DeepSeek provider adapter (OpenAI-compatible API).

CHIAMATE LLM: NON eseguite da questo adapter. Dopo il consolidamento del
trasporto (regola L / ADR 0026) tutte le chiamate (generate / agent turn)
passano dal ``GatewayProvider`` (delega al gateway Rust). Questo adapter resta
costruito SOLO per i metodi NON-chiamata ereditati da
``OpenAICompatProviderBase``: ``list_models`` (catalog-sync) e ``test_connection``
(health-check admin), piu' il client SDK on-demand che essi usano. Le quirk
DeepSeek delle chiamate (thinking disabled per tool/task interni, sanitizzazione
leak DSML, reasoning_content round-trip, telemetria context-cache) vivono ora nel
gateway Rust (crates/nexus-gateway/src/providers/).
"""
from __future__ import annotations

import logging

from .base import OpenAICompatProviderBase

logger = logging.getLogger(__name__)

BASE_URL = "https://api.deepseek.com/v1"


class DeepSeekProvider(OpenAICompatProviderBase):
    """Provider DeepSeek (OpenAI-compatible endpoint).

    Plumbing comune (API key/client, catalogo da DB, guard key mancante,
    test_connection) nel punto unico ``OpenAICompatProviderBase``
    (regola L / ADR 0026, Wave E3).
    """

    name = "deepseek"
    base_url = BASE_URL
    api_key_label = "DeepSeek"
