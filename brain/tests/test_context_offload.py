"""Test offload RAG lossless del contesto LLM (mig 0217).

Verifica il principio non negoziabile: NESSUN contenuto viene PERSO quando il
brain tronca/comprime un tool result. Prima del taglio, il contenuto COMPLETO
deve essere indicizzato in RAG (Qdrant), idempotente per hash.

Copre:
  (a) un tool result grande viene indicizzato in RAG PRIMA di essere ridotto nel
      prompt, e il puntatore lasciato cita nexus_search_semantic;
  (b) i settings sono DB-driven con default sicuri su DB down;
  (c) nessun percorso tronchi-e-butti senza prima offload (compressione vecchi
      messaggi inclusa);
  (d) idempotenza: re-indicizzare lo stesso contenuto produce gli stessi point id.

Mock puri: nessuna connessione DB o Qdrant reale. Il servizio embedding/Qdrant
e' fakeato in memoria.
"""
import unittest
from typing import Any

from brain.agents import context_offload


class _FakeVector:
    def __init__(self, values: list[float]) -> None:
        self.values = values


class _FakeQdrant:
    """Qdrant in-memory: cattura upsert per ispezione."""

    def __init__(self) -> None:
        self.collections_created: list[str] = []
        self.upserts: list[dict[str, Any]] = []

    def get_collections(self):
        class _C:
            def __init__(self, names):
                self.collections = [type("X", (), {"name": n})() for n in names]

        return _C(self.collections_created)

    def create_collection(self, collection_name: str, vectors_config: Any) -> None:
        self.collections_created.append(collection_name)

    def upsert(self, collection_name: str, points: list[Any]) -> None:
        self.upserts.append({"collection": collection_name, "points": points})


class _FakeEmbeddings:
    """EmbeddingService fake: embed deterministico, espone _get_qdrant."""

    _dimension = 4

    def __init__(self, qdrant: _FakeQdrant | None) -> None:
        self._qdrant = qdrant

    def _get_qdrant(self) -> Any:
        return self._qdrant

    def embed_batch(self, model: str, texts: list[str]) -> list[_FakeVector]:
        return [_FakeVector([float(len(t) % 7)] * self._dimension) for t in texts]


def _force_config(**overrides: Any) -> None:
    """Forza la cache config dell'offload con valori noti (no DB)."""
    cfg = dict(context_offload._OFFLOAD_DEFAULTS)
    cfg.update(overrides)
    context_offload._OFFLOAD_CACHE["config"] = cfg
    context_offload._OFFLOAD_CACHE["loaded_at"] = 1e18  # mai scade nel test


class TestOffloadToRag(unittest.TestCase):
    def setUp(self) -> None:
        # Config deterministica: offload attivo, soglia bassa per testare.
        _force_config(
            rag_offload_enabled=True,
            offload_min_chars=100,
            chunk_size=200,
            chunk_overlap=50,
            collection_tool_results="tool_results_chunks",
            max_chunks_per_item=500,
        )

    def tearDown(self) -> None:
        context_offload._OFFLOAD_CACHE["config"] = None
        context_offload._OFFLOAD_CACHE["loaded_at"] = 0.0

    def test_a_contenuto_grande_indicizzato_prima_del_taglio(self) -> None:
        """(a) tool result grande -> chunk indicizzati in Qdrant + ref ritornato."""
        qdrant = _FakeQdrant()
        emb = _FakeEmbeddings(qdrant)
        big = "X" * 5000

        result = context_offload.offload_to_rag(emb, big, source_kind="tool_result")

        self.assertIsNotNone(result)
        assert result is not None
        self.assertEqual(result["chars"], 5000)
        self.assertGreater(result["chunks"], 1)
        # I point sono stati effettivamente upsertati nella collection giusta.
        self.assertEqual(len(qdrant.upserts), 1)
        self.assertEqual(qdrant.upserts[0]["collection"], "tool_results_chunks")
        self.assertEqual(len(qdrant.upserts[0]["points"]), result["chunks"])
        # Il payload del primo chunk contiene il testo e l'hash (recuperabile).
        p0 = qdrant.upserts[0]["points"][0]
        self.assertEqual(p0.payload["content_hash"], result["content_hash"])
        self.assertEqual(p0.payload["source_kind"], "tool_result")
        self.assertIn("text", p0.payload)

    def test_a_pointer_cita_nexus_search_semantic(self) -> None:
        """Il puntatore lasciato nel prompt deve istruire il recupero via RAG."""
        offload = {"ref": "abc123def456", "chunks": 3, "chars": 5000}
        pointer = context_offload.build_pointer(5000, offload)
        self.assertIn("INDICIZZATO in RAG", pointer)
        self.assertIn("nexus_search_semantic", pointer)
        self.assertIn("abc123def456", pointer)

    def test_b_default_sicuri_se_db_down(self) -> None:
        """(b) DB down -> _load_offload_config ritorna defaults safe, non solleva."""
        context_offload._OFFLOAD_CACHE["config"] = None
        context_offload._OFFLOAD_CACHE["loaded_at"] = 0.0
        # Nessun DATABASE_URL valido nell'ambiente di test -> ramo eccezione.
        cfg = context_offload._load_offload_config()
        self.assertTrue(cfg["rag_offload_enabled"])
        self.assertEqual(cfg["rag_top_k"], context_offload._OFFLOAD_DEFAULTS["rag_top_k"])
        self.assertEqual(
            cfg["collection_tool_results"],
            context_offload._OFFLOAD_DEFAULTS["collection_tool_results"],
        )

    def test_b_offload_disabilitato_non_indicizza(self) -> None:
        """Flag master off -> nessun offload (degrada al vecchio comportamento)."""
        _force_config(rag_offload_enabled=False, offload_min_chars=100)
        qdrant = _FakeQdrant()
        emb = _FakeEmbeddings(qdrant)
        result = context_offload.offload_to_rag(emb, "X" * 5000, source_kind="tool_result")
        self.assertIsNone(result)
        self.assertEqual(len(qdrant.upserts), 0)

    def test_c_qdrant_down_non_perde_silenziosamente(self) -> None:
        """(c) Qdrant down -> ritorna None ma il pointer segnala il degrado.

        Il chiamante tronca comunque (necessario per il context window) ma il
        puntatore dice esplicitamente che l'offload non era disponibile: niente
        perdita silenziosa.
        """
        emb = _FakeEmbeddings(None)  # _get_qdrant() -> None
        result = context_offload.offload_to_rag(emb, "X" * 5000, source_kind="tool_result")
        self.assertIsNone(result)
        pointer = context_offload.build_pointer(5000, result)
        self.assertIn("offload RAG non disponibile", pointer)

    def test_c_contenuto_piccolo_non_indicizzato(self) -> None:
        """Sotto min_chars il contenuto sta gia' intero nel prompt: niente offload."""
        qdrant = _FakeQdrant()
        emb = _FakeEmbeddings(qdrant)
        result = context_offload.offload_to_rag(emb, "piccolo", source_kind="tool_result")
        self.assertIsNone(result)
        self.assertEqual(len(qdrant.upserts), 0)

    def test_d_idempotenza_point_id_deterministico(self) -> None:
        """(d) Re-indicizzare lo stesso contenuto -> stessi point id (upsert no-op)."""
        big = "Y" * 5000
        q1, q2 = _FakeQdrant(), _FakeQdrant()
        r1 = context_offload.offload_to_rag(_FakeEmbeddings(q1), big, source_kind="tool_result")
        r2 = context_offload.offload_to_rag(_FakeEmbeddings(q2), big, source_kind="tool_result")
        assert r1 is not None and r2 is not None
        self.assertEqual(r1["content_hash"], r2["content_hash"])
        ids1 = [p.id for p in q1.upserts[0]["points"]]
        ids2 = [p.id for p in q2.upserts[0]["points"]]
        self.assertEqual(ids1, ids2)


class TestSmartTruncateLossless(unittest.TestCase):
    """Integrazione con nodes.py: _smart_truncate_lossless offloada prima di tagliare."""

    def setUp(self) -> None:
        _force_config(rag_offload_enabled=True, offload_min_chars=100,
                      chunk_size=200, chunk_overlap=50,
                      collection_tool_results="tool_results_chunks",
                      max_chunks_per_item=500)

    def tearDown(self) -> None:
        context_offload._OFFLOAD_CACHE["config"] = None
        context_offload._OFFLOAD_CACHE["loaded_at"] = 0.0

    def test_truncate_lossless_indicizza_e_lascia_pointer(self) -> None:
        from brain.agents import nodes

        qdrant = _FakeQdrant()
        emb = _FakeEmbeddings(qdrant)
        original = nodes._embeddings
        nodes._embeddings = emb  # inietta il fake
        try:
            big = "Z" * 8000
            out = nodes._smart_truncate_lossless(big, max_chars=1000, source_kind="tool_result")
            # Il contenuto e' stato indicizzato PRIMA del taglio.
            self.assertEqual(len(qdrant.upserts), 1)
            # L'output e' piu' corto dell'originale (taglio avvenuto).
            self.assertLess(len(out), len(big))
            # Il puntatore cita il recupero via RAG.
            self.assertIn("nexus_search_semantic", out)
        finally:
            nodes._embeddings = original

    def test_truncate_sotto_soglia_invariato(self) -> None:
        from brain.agents import nodes

        small = "ciao"
        self.assertEqual(nodes._smart_truncate_lossless(small, max_chars=1000), small)

    def test_truncate_preserva_la_coda_errori_build(self) -> None:
        """Per i build l'informazione critica (gli ultimi errori + 'Found N
        errors') sta in CODA: il troncamento testa+coda DEVE preservarla, non
        decapitarla. Regressione del bug "l'agente ripete npm run build" perche'
        vede solo i primi errori e perde la coda + il totale.
        """
        from brain.agents import nodes

        qdrant = _FakeQdrant()
        emb = _FakeEmbeddings(qdrant)
        original = nodes._embeddings
        nodes._embeddings = emb  # inietta il fake
        try:
            # Output simil-tsc: molti errori, con il totale IN FONDO.
            errori = "\n".join(f"src/f{i}.ts(10,5): error TS2304: msg" for i in range(400))
            big = errori + "\n\nFound 400 errors in 12 files.\n"
            out = nodes._smart_truncate_lossless(big, max_chars=2000, source_kind="tool_result")
            # La CODA (il totale finale) sopravvive al troncamento.
            self.assertIn("Found 400 errors", out)
            # Il troncamento e' comunque avvenuto (non e' passato tutto).
            self.assertLess(len(out), len(big))
        finally:
            nodes._embeddings = original

    def test_cap_piu_alto_preserva_piu_coda(self) -> None:
        """Cap DB-driven: un tool_result_max_chars piu' grande (capability del
        provider agentico) preserva PIU' coda. Prova che applicare il cap dalla
        capability (campo prima morto) ha effetto reale sulla coda errori.
        """
        from brain.agents import nodes

        qdrant = _FakeQdrant()
        emb = _FakeEmbeddings(qdrant)
        original = nodes._embeddings
        nodes._embeddings = emb
        try:
            big = "".join(f"riga-errore-{i:05d}\n" for i in range(3000))
            out_piccolo = nodes._smart_truncate_lossless(big, max_chars=2000)
            out_grande = nodes._smart_truncate_lossless(big, max_chars=16000)
            # Entrambi finiscono con la stessa coda (testa+coda preserva il fondo),
            # ma il cap maggiore mantiene piu' contenuto complessivo.
            self.assertTrue(big.endswith("riga-errore-02999\n"))
            self.assertIn("riga-errore-02999", out_piccolo)
            self.assertIn("riga-errore-02999", out_grande)
            self.assertGreater(len(out_grande), len(out_piccolo))
        finally:
            nodes._embeddings = original


if __name__ == "__main__":
    unittest.main()
