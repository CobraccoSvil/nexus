"""Regressione: il client dei provider OpenAI-compatibili deve essere LOOP-AWARE.

Bug: il gRPC del brain e' sincrono e usa asyncio.run() per ogni RPC
(neural_service), creando e CHIUDENDO un event loop ad ogni chiamata. Il client
httpx async era cachato e riusato tra loop diversi -> al 2o uso le connessioni
nel pool erano legate al loop chiuso -> RuntimeError('Event loop is closed') ->
APIConnectionError -> il provider risultava 'non raggiungibile' e finiva in
cooldown anche con rete e credito OK.

Fix: _get_client ricrea il client quando l'event loop corrente cambia.
"""
import asyncio

from brain.providers.base import ApiKeyClientMixin


class _FakeProvider(ApiKeyClientMixin):
    name = "fake"

    def __init__(self):
        self._api_key_provider = lambda: "k"
        self._client = None
        self._cached_key = "k"
        self._client_loop = None
        self.creations = 0

    def _create_client(self, api_key):
        self.creations += 1
        return object()


def test_ricrea_il_client_quando_il_loop_cambia():
    p = _FakeProvider()

    async def get():
        return p._get_client()

    c1 = asyncio.run(get())   # loop 1
    assert p.creations == 1
    c2 = asyncio.run(get())   # loop 2 (nuovo, il precedente e' chiuso)
    assert p.creations == 2, "il client va ricreato quando il loop cambia"
    assert c1 is not c2


def test_riusa_il_client_nello_stesso_loop():
    p = _FakeProvider()

    async def two_calls():
        a = p._get_client()
        b = p._get_client()
        return a, b

    a, b = asyncio.run(two_calls())
    assert a is b, "stesso loop -> stesso client (pooling intra-RPC preservato)"
    assert p.creations == 1
