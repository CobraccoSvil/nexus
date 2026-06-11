"""Embedding service with sentence-transformers and Qdrant vector store."""
from __future__ import annotations

import logging
import os
from dataclasses import dataclass, field
from typing import Any

import numpy as np

logger = logging.getLogger(__name__)

# Lazy-load heavy dependencies
_model_cache: dict[str, Any] = {}


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
    """Real embedding service using sentence-transformers + Qdrant."""

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

    def _get_model(self, model_name: str) -> Any:
        if model_name not in _model_cache:
            try:
                from sentence_transformers import SentenceTransformer
                logger.info("Loading sentence-transformers model: %s", model_name)
                _model_cache[model_name] = SentenceTransformer(model_name)
            except ImportError:
                logger.warning("sentence-transformers not installed, using fallback embeddings")
                _model_cache[model_name] = None
        return _model_cache[model_name]

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

    def _fallback_embed(self, text: str) -> list[float]:
        """Deterministic fallback when sentence-transformers is unavailable."""
        base = [float((ord(ch) % 13) / 13) for ch in text[:self._dimension]]
        padded = base + [0.0] * max(0, self._dimension - len(base))
        return padded[:self._dimension]

    def embed_text(self, model: str, text: str) -> EmbeddingVector:
        model_name = model or self._default_model
        st_model = self._get_model(model_name)

        if st_model is not None:
            embedding = st_model.encode(text, normalize_embeddings=True)
            values = embedding.tolist()
        else:
            values = self._fallback_embed(text)

        return EmbeddingVector(model=model_name, values=values)

    def embed_batch(self, model: str, texts: list[str]) -> list[EmbeddingVector]:
        model_name = model or self._default_model
        st_model = self._get_model(model_name)

        if st_model is not None:
            embeddings = st_model.encode(texts, normalize_embeddings=True, batch_size=32)
            return [
                EmbeddingVector(model=model_name, values=emb.tolist())
                for emb in embeddings
            ]
        return [self.embed_text(model_name, t) for t in texts]

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
