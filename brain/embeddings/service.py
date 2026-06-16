"""Embedding service: thin client verso l'embedder ONNX di mcp-core.

Cutover Fase 2 (studio brain->Rust): il modello vive UNA sola volta, in Rust
(nexus-orchestrator OnnxMiniLmEmbedder, esposto da mcp-core POST /api/embed). Il
brain NON carica piu' SentenceTransformer/PyTorch -> -300/400 MB RSS e ~55 thread
BLAS in meno. Parita' vettoriale verificata (cosine 1.0 vs PyTorch), quindi i
vettori gia' indicizzati in Qdrant restano validi (nessun re-index).
"""
from __future__ import annotations

import logging
import os
from dataclasses import dataclass, field
from typing import Any

import httpx

logger = logging.getLogger(__name__)


def _mcp_core_url() -> str:
    """URL base di mcp-core (punto unico embedder). Env > settings DB > default."""
    env = os.environ.get("MCP_CORE_URL")
    if env:
        return env.rstrip("/")
    try:
        from brain.utils.settings_db import get_setting
        return get_setting("mcp_core_url", "http://127.0.0.1:4000").rstrip("/")
    except Exception:
        return "http://127.0.0.1:4000"


@dataclass(slots=True)
class EmbeddingVector:
    model: str
    values: list[float]


@dataclass(slots=True)
class SearchResult:
    id: str
    score: float
    payload: dict[str, Any] = field(default_factory=dict)


class EmbeddingService:
    """Embedding via mcp-core ONNX (HTTP /api/embed) + Qdrant per store/search."""

    def __init__(
        self,
        default_model: str = "all-MiniLM-L6-v2",
        qdrant_url: str | None = None,
        qdrant_collection: str = "code_embeddings",
    ) -> None:
        self._default_model = default_model
        if qdrant_url:
            self._qdrant_url = qdrant_url
        else:
            env = os.getenv("QDRANT_URL")
            if env:
                self._qdrant_url = env
            else:
                from brain.utils.settings_db import get_setting
                self._qdrant_url = get_setting("qdrant_url", "http://localhost:6333")
        self._collection = qdrant_collection
        self._qdrant_client: Any | None = None
        self._dimension = 384  # all-MiniLM-L6-v2 outputs 384-dim vectors
        self._embed_url = f"{_mcp_core_url()}/api/embed"

    def _get_qdrant(self) -> Any | None:
        if self._qdrant_client is None:
            try:
                from qdrant_client import QdrantClient
                self._qdrant_client = QdrantClient(url=self._qdrant_url, timeout=5)
                self._ensure_collection()
            except Exception as e:
                logger.warning("Qdrant not available: %s", e)
                self._qdrant_client = None
        return self._qdrant_client

    def _ensure_collection(self) -> None:
        client = self._qdrant_client
        if client is None:
            return
        try:
            from qdrant_client.models import Distance, VectorParams
            collections = [c.name for c in client.get_collections().collections]
            if self._collection not in collections:
                client.create_collection(
                    collection_name=self._collection,
                    vectors_config=VectorParams(size=self._dimension, distance=Distance.COSINE),
                )
                logger.info("Created Qdrant collection: %s", self._collection)
        except Exception as e:
            logger.warning("Failed to ensure collection: %s", e)

    def embed_text(self, model: str, text: str) -> EmbeddingVector:
        model_name = model or self._default_model
        try:
            resp = httpx.post(
                self._embed_url,
                json={"model": model_name, "text": text},
                timeout=15.0,
            )
            resp.raise_for_status()
            data = resp.json()
            values = data.get("vector") or []
        except Exception as e:  # errore visibile (regola G: niente fallback nascosto)
            raise RuntimeError(
                f"embed_text via mcp-core fallita ({self._embed_url}): {e}"
            ) from e
        if not values:
            raise RuntimeError(
                f"embed_text: vettore vuoto da mcp-core (modello {model_name})"
            )
        return EmbeddingVector(
            model=str(data.get("model") or model_name),
            values=[float(x) for x in values],
        )

    def embed_batch(self, model: str, texts: list[str]) -> list[EmbeddingVector]:
        model_name = model or self._default_model
        if not texts:
            return []
        try:
            resp = httpx.post(
                self._embed_url,
                json={"model": model_name, "texts": texts},
                timeout=60.0,
            )
            resp.raise_for_status()
            data = resp.json()
            vectors = data.get("vectors") or []
        except Exception as e:  # errore visibile (regola G)
            raise RuntimeError(
                f"embed_batch via mcp-core fallita ({self._embed_url}): {e}"
            ) from e
        if len(vectors) != len(texts):
            raise RuntimeError(
                f"embed_batch: mismatch vettori={len(vectors)} testi={len(texts)}"
            )
        used_model = str(data.get("model") or model_name)
        return [
            EmbeddingVector(model=used_model, values=[float(x) for x in v])
            for v in vectors
        ]

    def store_vectors(
        self,
        ids: list[str],
        vectors: list[list[float]],
        payloads: list[dict[str, Any]] | None = None,
    ) -> bool:
        client = self._get_qdrant()
        if client is None:
            logger.warning("Qdrant unavailable, vectors not stored")
            return False

        try:
            from qdrant_client.models import PointStruct
            points = [
                PointStruct(
                    id=i,
                    vector=vec,
                    payload=payloads[idx] if payloads else {},
                )
                for idx, (i, vec) in enumerate(zip(ids, vectors))
            ]
            client.upsert(collection_name=self._collection, points=points)
            return True
        except Exception as e:
            logger.error("Failed to store vectors: %s", e)
            return False

    def search_similar(
        self,
        query_vector: list[float],
        top_k: int = 5,
    ) -> list[SearchResult]:
        client = self._get_qdrant()
        if client is None:
            return []

        try:
            results = client.query_points(
                collection_name=self._collection,
                query=query_vector,
                limit=top_k,
            )
            return [
                SearchResult(
                    id=str(hit.id),
                    score=hit.score,
                    payload=hit.payload or {},
                )
                for hit in results.points
            ]
        except Exception as e:
            logger.error("Search failed: %s", e)
            return []

    def semantic_search(self, query: str, top_k: int = 5) -> list[SearchResult]:
        """Search indexed code by semantic similarity."""
        query_vec = self.embed_text(self._default_model, query)
        return self.search_similar(query_vec.values, top_k)

    @staticmethod
    def _chunk_text(text: str, chunk_size: int) -> list[str]:
        """Chunker testuale del servizio embeddings con feature flag DB-driven
        (mig 0326). Setting ``rag.chunker.algorithm``:

          - ``legacy`` (default): split per linee greedy, NO overlap. I chunk
            corrispondono a quelli gia' indicizzati in Qdrant. Valore safe.
          - ``unified``: delega a ``brain.utils.text_chunk.chunk_text``
            (paritetico al Rust ``rag/chunker.rs``, sliding window char con
            overlap e smart trimming su whitespace). ATTIVABILE SOLO DOPO
            re-index della collection Qdrant — vedi migrazione 0326.

        Lettura del flag best-effort: se il DB e' down si comporta come
        ``legacy`` (no regressione su path caldi).
        """
        try:
            from brain.utils.settings_db import get_setting
            algo = get_setting("rag.chunker.algorithm", "legacy").strip().lower()
        except Exception:
            algo = "legacy"

        if algo == "unified":
            # Punto unico paritetico col Rust (regola L / ADR 0026, Wave 8a).
            # Overlap 0 per restare semantica-friendly al posto della split-per-
            # linea storica: l'admin che attiva il flag lo fa appositamente con
            # re-index, eventuali parametri size/overlap si tunano dal DB
            # (settings agent.rag.chunk_size / chunk_overlap, vedi context_offload).
            from brain.utils.text_chunk import chunk_text
            return chunk_text(text, chunk_size, 0)

        # ── legacy: algoritmo storico (split-per-linea, NO overlap) ──
        lines = text.split("\n")
        chunks: list[str] = []
        current: list[str] = []
        current_len = 0

        for line in lines:
            line_len = len(line) + 1
            if current_len + line_len > chunk_size and current:
                chunks.append("\n".join(current))
                current = [line]
                current_len = line_len
            else:
                current.append(line)
                current_len += line_len

        if current:
            chunks.append("\n".join(current))
        return chunks
