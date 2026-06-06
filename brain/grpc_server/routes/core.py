"""Endpoint REST di base: health, classify, routing, embed, search, providers,
complete e reload-settings.

Lo stato condiviso (router semantico, embedding service, provider registry,
classifier agentic) vive in `brain.grpc_server.runtime` e viene letto via
attributo di modulo (`runtime.providers`, ...) per vedere sempre lo stato
inizializzato allo startup.
"""
from __future__ import annotations

import logging

from fastapi import APIRouter, HTTPException
from pydantic import BaseModel

from brain.grpc_server import runtime

logger = logging.getLogger(__name__)

router = APIRouter()


class IntentRequest(BaseModel):
    project_id: str
    profile_id: str
    message: str


class AgenticIntentRequest(BaseModel):
    """Schema snello per `/classify-intent-agentic`: solo il messaggio. Niente
    project_id/profile_id perche' la classificazione e' puramente testuale e
    cacheable cross-project."""
    message: str


class CompletionRequest(BaseModel):
    provider: str
    model: str
    prompt: str


class EmbedRequest(BaseModel):
    model: str = ""
    text: str = ""
    texts: list[str] = []


class SearchRequest(BaseModel):
    query: str
    top_k: int = 5


class ReloadSettingsRequest(BaseModel):
    mcp_core_url: str = "http://localhost:4000"


@router.get("/health")
def health() -> dict[str, str]:
    return {"service": "neural-core", "status": "ok", "version": "0.2.0"}


@router.get("/providers/billing-cooldown")
def billing_cooldown() -> dict[str, dict[str, int]]:
    """Snapshot dei provider in billing-error cooldown locale (brain-side).

    Usato da mcp-core per aggiornare lo stato canonico (LED UI gialli).
    Quando un provider ritorna `billing_error` (credit_balance_too_low) il
    brain lo mette in cache per evitare chiamate API sprecate. Questo
    endpoint espone la cache cosi' il LED UI riflette la realta'.

    Output: { providers: { "anthropic": 540, "openai": 0 } } — secondi rimanenti.
    """
    from brain.providers.registry import get_billing_cooldown_snapshot
    return {"providers": get_billing_cooldown_snapshot()}


@router.post("/classify-intent")
async def classify_intent(body: IntentRequest) -> dict[str, str]:
    # Interpretazione semantica via LLM (niente piu' keyword). Manteniamo la
    # forma di output {intent, confidence} per retrocompatibilita' dei client.
    result = await runtime.agentic_classifier.classify(body.message)
    return {"intent": result.intent, "confidence": f"{result.confidence:.2f}"}


@router.post("/classify-intent-agentic")
async def classify_intent_agentic(body: AgenticIntentRequest) -> dict[str, object]:
    """Classifier LLM-based con output JSON strutturato (Fase 2).

    Output:
      - intent: come /classify-intent
      - agentic_score: 0..1, probabilita' che il task richieda tool use multi-step
      - requires_tools: bool, hint per la UI / routing
      - complexity: low/medium/high, per scelta tier modello
      - confidence: 0..1, fiducia LLM
      - model_used: modello che ha classificato
      - cached: true se il risultato viene dalla cache TTL 24h
      - fallback_used: true se LLM fallito ed e' stato usato l'intent neutro `agentic_default`

    Cache: in-memory TTL 24h, key=sha256(message[:1000]).
    """
    result = await runtime.agentic_classifier.classify(body.message)
    return result.to_dict()


@router.get("/classify-intent-agentic/stats")
async def classify_intent_agentic_stats() -> dict[str, object]:
    """Stato cache e configurazione classifier — utile per monitoring."""
    return await runtime.agentic_classifier.stats()


@router.post("/route-model")
async def route_model(body: IntentRequest) -> dict[str, str]:
    classification = await runtime.agentic_classifier.classify(body.message)
    intent = classification.intent
    # route_model e' un thin-client: passa il message al routing Rust, che
    # decide provider/model (e ri-usa il classifier LLM via cache).
    decision = runtime.router.route_model(intent, token_budget=4096, message=body.message)
    return {
        "intent": intent,
        "provider": decision.provider,
        "model": decision.model,
        "rationale": decision.rationale,
        "confidence": str(decision.confidence),
    }


@router.post("/embed")
def embed(body: EmbedRequest) -> dict[str, object]:
    if body.texts:
        vectors = runtime.embeddings.embed_batch(body.model, body.texts)
        return {"model": body.model, "vectors": [v.values for v in vectors], "count": len(vectors)}
    vector = runtime.embeddings.embed_text(body.model, body.text)
    return {"model": vector.model, "vector": vector.values, "dimensions": len(vector.values)}


@router.post("/search")
def semantic_search(body: SearchRequest) -> dict[str, object]:
    results = runtime.embeddings.semantic_search(body.query, body.top_k)
    return {
        "query": body.query,
        "results": [{"id": r.id, "score": r.score, "payload": r.payload} for r in results],
    }


@router.get("/providers/{provider}/models")
def list_models(provider: str) -> dict[str, object]:
    return runtime.providers.sync_models(provider)


@router.get("/providers/{provider}/models/live")
async def list_models_live(provider: str) -> dict[str, object]:
    """Lista modelli realmente disponibili sull'API del provider (autodiscovery live).

    Diverso da /providers/{p}/models che legge dal catalog DB: questo endpoint
    chiama il SDK del provider e ritorna i modelli effettivamente esposti
    dall'API in questo momento. Usato da catalog_sync_loop (Rust mcp-core) per
    provider che richiedono auth complessa (Vertex SDK con Service Account):
    il worker Rust non puo' chiamare Vertex direct (auth Google complica), ma
    chiama questo endpoint che gira Python con il SDK gia' configurato dal DB.

    Output: { "provider": "google", "models": ["gemini-2.5-flash", ...] }
    Su errore: HTTP 503 con { "error": "..." }.
    """
    prov = runtime.providers._providers.get(provider)
    if prov is None:
        raise HTTPException(status_code=404, detail=f"provider '{provider}' non noto")
    # Provider Google: chiama il SDK Vertex/Gemini reale
    if provider == "google":
        try:
            from brain.providers.google_provider import GoogleProvider
            assert isinstance(prov, GoogleProvider)
            ok, reason = prov._is_configured()
            if not ok:
                from fastapi import HTTPException as _HTTPException
                raise _HTTPException(status_code=503, detail=f"google non configurato: {reason}")
            client = prov._get_client()
            # client.aio.models.list() ritorna AsyncPager iterabile
            model_names: list[str] = []
            page = await client.aio.models.list()
            async for m in page:
                # Vertex ritorna "publishers/google/models/gemini-2.5-flash"
                # Gemini direct ritorna "models/gemini-2.5-flash"
                # Normalizziamo a basename per coerenza col catalog DB.
                name = (m.name or "").rsplit("/", 1)[-1]
                if name:
                    model_names.append(name)
            return {"provider": provider, "models": sorted(set(model_names))}
        except Exception as exc:
            logger.exception("list_models_live(google) fallito: %s", exc)
            raise HTTPException(status_code=503, detail=str(exc)) from exc
    # Altri provider: non implementato qui (Rust worker chiama API direct).
    raise HTTPException(
        status_code=501,
        detail=f"live listing per '{provider}' non implementato (usa /v1/models del provider direct)",
    )


@router.get("/providers/{provider}/health")
async def provider_health(provider: str) -> dict[str, object]:
    return await runtime.providers.test_connection_async(provider)


@router.post("/complete")
async def complete(body: CompletionRequest) -> dict[str, object]:
    result = await runtime.providers.generate_completion_async(body.provider, body.model, body.prompt)
    return {
        "provider": result.provider,
        "model": result.model,
        "content": result.content,
        "metadata": result.metadata,
    }


@router.post("/reload-settings")
def reload_settings(body: ReloadSettingsRequest) -> dict[str, object]:
    return runtime._load_keys_from_db()
