"""Recupero di interazioni simili via similarity search su Qdrant."""
from __future__ import annotations

import logging
from typing import Any

logger = logging.getLogger(__name__)


class InteractionRetriever:
    """Recupera interazioni simili dal vector store Qdrant per RAG contestuale."""

    COLLECTION = "agent_interactions"

    def __init__(self, embedding_service: Any) -> None:
        self._embeddings = embedding_service

    def _ensure_collection(self) -> None:
        """Crea la collection Qdrant se non esiste."""
        client = self._embeddings._get_qdrant()
        if client is None:
            return
        try:
            from qdrant_client.models import Distance, VectorParams  # type: ignore[import-untyped]

            collections = [c.name for c in client.get_collections().collections]
            if self.COLLECTION not in collections:
                client.create_collection(
                    collection_name=self.COLLECTION,
                    vectors_config=VectorParams(
                        size=self._embeddings._dimension,
                        distance=Distance.COSINE,
                    ),
                )
                logger.info("Qdrant collection creata: %s", self.COLLECTION)
        except Exception as exc:
            logger.warning("Impossibile creare collection Qdrant: %s", exc)

    def store_interaction_vector(
        self,
        qdrant_id: str,
        text: str,
        payload: dict[str, Any],
    ) -> bool:
        """Genera embedding e salva il vettore in Qdrant."""
        self._ensure_collection()
        client = self._embeddings._get_qdrant()
        if client is None:
            logger.warning("Qdrant non disponibile, vettore non salvato")
            return False

        try:
            from qdrant_client.models import PointStruct  # type: ignore[import-untyped]

            vector = self._embeddings.embed_text("", text)
            point = PointStruct(id=qdrant_id, vector=vector.values, payload=payload)
            client.upsert(collection_name=self.COLLECTION, points=[point])
            return True
        except Exception as exc:
            logger.error("Errore salvataggio vettore Qdrant: %s", exc)
            return False

    def get_similar_interactions(
        self,
        query_text: str,
        task_type: str | None = None,
        limit: int = 5,
    ) -> list[dict[str, Any]]:
        """Recupera le interazioni più simili alla query per RAG."""
        client = self._embeddings._get_qdrant()
        if client is None:
            return []

        try:
            query_vec = self._embeddings.embed_text("", query_text)
            filter_condition = None
            if task_type:
                from qdrant_client.models import Filter, FieldCondition, MatchValue  # type: ignore[import-untyped]

                filter_condition = Filter(
                    must=[FieldCondition(key="task_type", match=MatchValue(value=task_type))]
                )

            results = client.query_points(
                collection_name=self.COLLECTION,
                query=query_vec.values,
                query_filter=filter_condition,
                limit=limit,
            )
            return [
                {
                    "id": str(hit.id),
                    "score": hit.score,
                    **hit.payload,
                }
                for hit in results.points
            ]
        except Exception as exc:
            logger.error("Errore similarity search: %s", exc)
            return []
