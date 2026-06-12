"""Regressione: detection famiglia OpenAI per max_completion_tokens / responses-only.

Bug originale: `_O_SERIES_MODELS` era una lista esatta che includeva gpt-5,
gpt-5.2..gpt-5.5 ma NON gpt-5.1 -> il brain inviava `max_tokens` ai gpt-5.1
-> 400 "Unsupported parameter: 'max_tokens' ... Use 'max_completion_tokens'"
-> model_health_probe auto-disabilitava e metteva openai in cooldown.

Fix: regola di famiglia per prefisso (gpt-5*, gpt-4.5*, o-series), cosi' ogni
release futura e' coperta senza aggiornare una lista (regola G).
"""
from brain.providers.openai_provider import _is_o_series, _is_responses_only


class TestIsOSeries:
    def test_gpt_5_1_e_coperto(self):
        # Il buco originale: gpt-5.1 e le sue varianti datate / chat-latest.
        assert _is_o_series("gpt-5.1")
        assert _is_o_series("gpt-5.1-2025-11-13")
        assert _is_o_series("gpt-5.1-chat-latest")

    def test_intera_famiglia_gpt5(self):
        for m in ("gpt-5", "gpt-5-mini", "gpt-5-nano", "gpt-5.2", "gpt-5.4",
                  "gpt-5.6", "gpt-5.9-mini"):
            assert _is_o_series(m), m

    def test_gpt45_e_oseries(self):
        assert _is_o_series("gpt-4.5")
        assert _is_o_series("gpt-4.5-preview")
        assert _is_o_series("o1-mini")
        assert _is_o_series("o3")
        assert _is_o_series("o4-mini")

    def test_modelli_vecchia_gen_usano_max_tokens(self):
        for m in ("gpt-4o-mini", "gpt-4o", "gpt-4-turbo", "gpt-3.5-turbo"):
            assert not _is_o_series(m), m


class TestIsResponsesOnly:
    def test_varianti_pro_codex_della_famiglia(self):
        # gpt-5.1-codex dava 404 "This is not a chat model".
        assert _is_responses_only("gpt-5.1-codex")
        assert _is_responses_only("gpt-5-pro")
        assert _is_responses_only("gpt-5.4-pro")
        assert _is_responses_only("o3-deep-research")

    def test_chat_models_non_sono_responses_only(self):
        for m in ("gpt-5.1", "gpt-5", "gpt-5-mini", "gpt-4o-mini"):
            assert not _is_responses_only(m), m
