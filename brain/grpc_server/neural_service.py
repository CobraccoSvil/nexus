"""gRPC service implementation for NeuralCoreService."""
from __future__ import annotations

import json
import logging
import os
import time
from concurrent import futures

import grpc

from brain.grpc_server.generated import neural_core_pb2 as pb2
from brain.grpc_server.generated import neural_core_pb2_grpc as pb2_grpc

logger = logging.getLogger(__name__)


class DefaultModelUnavailable(Exception):
    """Sollevata quando non e' possibile risolvere il default model per un
    provider (DB irraggiungibile o provider non in `nexus_provider_default_model`).
    Niente fallback hardcoded: il caller deve ritornare errore esplicito."""
    pass


# Cache 60s del default model per provider (letto da nexus_provider_default_model).
_DEFAULT_MODEL_CACHE: dict[str, str] = {}
_DEFAULT_MODEL_CACHE_TS: float = 0.0


def _default_model_for_provider(provider: str) -> str:
    """Ritorna il modello di default per `provider` letto da DB (cache 60s).

    **Niente fallback hardcoded**. Solleva `DefaultModelUnavailable` se DB
    irraggiungibile o provider non configurato — il caller deve ritornare
    errore esplicito.
    """
    global _DEFAULT_MODEL_CACHE, _DEFAULT_MODEL_CACHE_TS
    now = time.time()
    if (now - _DEFAULT_MODEL_CACHE_TS) >= 60.0 or not _DEFAULT_MODEL_CACHE:
        try:
            import psycopg2  # type: ignore[import]
            db_url = os.environ.get(
                "DATABASE_URL",
                "postgres://nexus:nexus@localhost:5433/nexus?sslmode=disable",
            )
            with psycopg2.connect(db_url) as conn:
                with conn.cursor() as cur:
                    cur.execute(
                        "SELECT provider, model_id FROM nexus_provider_default_model"
                    )
                    rows = cur.fetchall()
            _DEFAULT_MODEL_CACHE = {p: m for (p, m) in rows}
            _DEFAULT_MODEL_CACHE_TS = now
        except Exception as e:
            raise DefaultModelUnavailable(
                f"DB irraggiungibile per leggere default model di '{provider}': {e}. "
                "Verifica Postgres e migrazione 0101."
            )
    if provider not in _DEFAULT_MODEL_CACHE:
        raise DefaultModelUnavailable(
            f"Provider '{provider}' non configurato in nexus_provider_default_model. "
            "Esegui INSERT con il modello desiderato."
        )
    return _DEFAULT_MODEL_CACHE[provider]

# These will be set by main.py before serve() is called
embeddings = None
router = None
providers = None


def _classify_provider_error(exc: Exception) -> tuple[str, str]:
    """Classifica errori provider e restituisce (error_class, messaggio_umano).
    error_class è usato da mcp-core per decidere retry/fallback."""
    text = str(exc)
    low = text.lower()
    if "resourceexhausted" in low or "message larger than" in low or "larger than max" in low:
        return ("payload_too_large", "Il provider AI ha rifiutato un payload troppo grande. Riduci il contesto e riprova.")
    if "unauthenticated" in low or "invalid api key" in low or "401" in low or "missing_api_key" in low or "missing api key" in low:
        return ("auth_error", "Credenziali del provider AI non valide o mancanti.")
    if "deadlineexceeded" in low or "timeout" in low or "timed out" in low:
        return ("timeout", "Il provider AI non ha risposto in tempo.")
    # Credito/quota ESAURITA: persistente (serve ricarica/billing), NON un rate
    # limit transiente. Va valutata PRIMA del check rate_limit perche'
    # insufficient_quota di OpenAI arriva come HTTP 429 (matcherebbe "429"): se
    # finisse in rate_limit verrebbe trattata come transiente (cooldown 60s) e il
    # provider verrebbe ritentato all'infinito invece di essere disabilitato.
    if (
        "credit balance" in low
        or "insufficient_quota" in low
        or "exceeded your current quota" in low
        or "plans & billing" in low
        or "billing" in low
    ):
        return ("billing", "Credito esaurito sul provider AI.")
    if "rate limit" in low or "too many requests" in low or ("429" in low and "quota" not in low):
        return ("rate_limit", "Limite di richieste del provider AI raggiunto. Riprova tra poco.")
    if "overloaded" in low or "529" in low or "could not serve" in low:
        return ("overloaded", "Provider AI sovraccarico. Riprova tra poco.")
    if "unavailable" in low or "connection refused" in low or "503" in low:
        return ("unavailable", "Provider AI momentaneamente non disponibile.")
    if "quota" in low:
        # "quota" generica senza i marcatori sopra: trattata come billing
        # (quota esaurita) per sicurezza, non come rate limit.
        return ("billing", "Credito esaurito sul provider AI.")
    return ("unknown", "Errore del provider AI. Controlla i log per i dettagli.")


def _humanize_provider_error(exc: Exception) -> str:
    """Wrapper retrocompatibile: restituisce solo il messaggio umano."""
    _, human = _classify_provider_error(exc)
    return human


class NeuralCoreServicer(pb2_grpc.NeuralCoreServiceServicer):
    def EmbedText(self, request, context):
        vector = embeddings.embed_text(request.model, request.text)
        return pb2.JsonResponse(json=json.dumps({
            "model": vector.model,
            "vector": vector.values,
            "dimensions": len(vector.values),
        }))

    def EmbedBatch(self, request, context):
        vectors = embeddings.embed_batch(request.model, list(request.texts))
        return pb2.JsonResponse(json=json.dumps({
            "model": request.model,
            "vectors": [v.values for v in vectors],
            "count": len(vectors),
        }))

    def ClassifyIntent(self, request, context):
        result = router.classify_intent(request.message)
        return pb2.JsonResponse(json=json.dumps(result))

    def RouteModel(self, request, context):
        decision = router.route_model(request.intent, token_budget=request.token_budget)
        return pb2.JsonResponse(json=json.dumps({
            "provider": decision.provider,
            "model": decision.model,
            "rationale": decision.rationale,
            "confidence": decision.confidence,
            "token_budget": request.token_budget,
        }))

    def GenerateCompletion(self, request, context):
        try:
            result = providers.generate_completion(request.provider, request.model, request.prompt)
            content = result.content or ""
            error_meta = result.metadata.get("error")
            error_class = ""
            if content.startswith("[Error:") or error_meta:
                raw = error_meta or content[len("[Error:"):].rstrip("]").strip()
                logger.error("Provider %s/%s error: %s", request.provider, request.model, raw)
                error_class, human = _classify_provider_error(Exception(raw))
                content = human
                error_meta = human
            return pb2.JsonResponse(json=json.dumps({
                "provider": result.provider,
                "model": result.model,
                "content": content,
                "metadata": result.metadata,
                "error": error_meta,
                "error_class": error_class if error_class else None,
            }))
        except Exception as e:
            logger.error("GenerateCompletion failed: %s", e)
            error_class, human = _classify_provider_error(e)
            return pb2.JsonResponse(json=json.dumps({
                "provider": request.provider,
                "model": request.model,
                "content": human,
                "metadata": {},
                "error": human,
                "error_class": error_class,
            }))

    def GenerateStructuredCompletion(self, request, context):
        result = providers.generate_completion(request.provider, request.model, request.prompt)
        return pb2.JsonResponse(json=json.dumps({
            "provider": result.provider,
            "model": result.model,
            "content": result.content,
            "schema": request.json_schema,
            "metadata": result.metadata,
        }))

    def ListProviderModels(self, request, context):
        data = providers.sync_models(request.provider)
        return pb2.JsonResponse(json=json.dumps(data))

    def SyncProviderModels(self, request, context):
        data = providers.sync_models(request.provider)
        return pb2.JsonResponse(json=json.dumps(data))

    def TestProviderConnection(self, request, context):
        data = providers.test_connection(request.provider)
        return pb2.JsonResponse(json=json.dumps(data))

    def SyncKnowledgeBundle(self, request, context):
        return pb2.JsonResponse(json=json.dumps({
            "status": "accepted",
            "provider": request.provider,
        }))

    def GenerateAgentTurn(self, request, context):
        """Esegue un turno agente con tool_use support. Utilizzato dal loop agente in Rust."""
        try:
            messages = json.loads(request.messages_json) if request.messages_json else []
            tools = json.loads(request.tools_json) if request.tools_json else []
            max_tokens = request.max_tokens if request.max_tokens else 4096
            system_text = request.system_text if request.system_text else ""
            logger.info(
                "AgentTurn: provider=%s model=%s system_text_len=%d tools=%d msgs=%d",
                request.provider, request.model, len(system_text), len(tools), len(messages),
            )
            result = providers.generate_agent_turn_sync(
                request.provider, request.model, messages, tools, max_tokens,
                system_text=system_text,
            )
            # Sanitizza errori grezzi: i provider scrivono "[Error: <raw>]" dentro content;
            # convertiamoli in messaggio italiano umano. Niente JSON/grpc status nell'UI.
            content = result.content or ""
            error_meta = result.metadata.get("error")
            error_class = ""
            if content.startswith("[Error:") or error_meta:
                raw = error_meta or content[len("[Error:"):].rstrip("]").strip()
                logger.error("Provider %s/%s error: %s", request.provider, request.model, raw)
                error_class, human = _classify_provider_error(Exception(raw))
                content = human
                error_meta = human
            return pb2.JsonResponse(json=json.dumps({
                "provider": result.provider,
                "model": result.model,
                "content": content,
                "stop_reason": result.metadata.get("stop_reason", "end_turn"),
                "tool_use_blocks": result.metadata.get("tool_use_blocks", []),
                "assistant_content": result.metadata.get("assistant_content", []),
                "usage": result.metadata.get("usage", {}),
                "error": error_meta,
                "error_class": error_class if error_class else None,
            }))
        except Exception as e:
            logger.error("GenerateAgentTurn failed: %s", e)
            error_class, human = _classify_provider_error(e)
            return pb2.JsonResponse(json=json.dumps({
                "content": human,
                "stop_reason": "error",
                "tool_use_blocks": [],
                "assistant_content": [],
                "usage": {},
                "error": human,
                "error_class": error_class,
            }))

    def GetProviderHealth(self, request, context):
        if request.provider == "system":
            return pb2.JsonResponse(json=json.dumps({
                "status": "ok",
                "service": "neural-core",
                "embeddings": "ready",
                "providers": list(providers._providers.keys()),
            }))
        data = providers.test_connection(request.provider)
        return pb2.JsonResponse(json=json.dumps(data))

    def SubmitBatchReview(self, request, context):
        """Submit a Gemini batch job for deep file review.
        request.json contains: {"files": [...], "api_key": str, "model": str}
        """
        import asyncio
        data = json.loads(request.json)
        files = data["files"]
        api_key = data.get("api_key", "")
        # Modello da DB (mig 0101). Errore esplicito se non configurato.
        try:
            model = data.get("model") or _default_model_for_provider("google")
        except DefaultModelUnavailable as e:
            return pb2.JsonResponse(json=json.dumps({"error": str(e)}))

        from brain.providers.google_batch import GoogleBatchClient
        client = GoogleBatchClient(api_key=api_key, model=model)

        try:
            job_name = asyncio.run(client.analyze_files_batch(files))
            return pb2.JsonResponse(json=json.dumps({"job_name": job_name, "status": "submitted"}))
        except Exception as e:
            return pb2.JsonResponse(json=json.dumps({"error": str(e)}))

    def GetBatchJobStatus(self, request, context):
        """Get status (and results if complete) of a Gemini batch job.
        request.json contains: {"job_name": str, "api_key": str, "model": str}
        """
        data = json.loads(request.json)
        job_name = data["job_name"]
        api_key = data.get("api_key", "")
        # Modello da DB (mig 0101). Errore esplicito se non configurato.
        try:
            model = data.get("model") or _default_model_for_provider("google")
        except DefaultModelUnavailable as e:
            return pb2.JsonResponse(json=json.dumps({"error": str(e)}))

        from brain.providers.google_batch import GoogleBatchClient
        client = GoogleBatchClient(api_key=api_key, model=model)

        try:
            status = client.get_job_status(job_name)
            if status["state"] == "JOB_STATE_SUCCEEDED":
                results = client.get_results(job_name)
                status["results"] = results
            return pb2.JsonResponse(json=json.dumps(status))
        except Exception as e:
            return pb2.JsonResponse(json=json.dumps({"error": str(e)}))


    def GenerateDocument(self, request, context):
        """Generate a .docx document from structured JSON content."""
        from brain.documents.generator import DocumentGenerator

        gen = DocumentGenerator()
        result = gen.generate(
            doc_type=request.doc_type,
            content_json=request.content_json,
            output_path=request.output_path,
            standard=request.standard or "ieee830",
            title=request.title or "",
            project_name=request.project_name or "",
        )

        if result.get("error"):
            logger.error("Document generation failed: %s", result["error"])
            return pb2.GenerateDocumentResponse(
                file_path="",
                page_count=0,
                section_count=0,
                error=result["error"],
            )

        logger.info("Document generated: %s (%d sections)", result["file_path"], result["section_count"])
        return pb2.GenerateDocumentResponse(
            file_path=result["file_path"],
            page_count=result["page_count"],
            section_count=result["section_count"],
            error="",
        )


def serve(port: int = 50051) -> None:
    # Default gRPC è 4MB; non basta per prompt grandi (file letti, context lunghi).
    # Alziamo a 128MB in entrambe le direzioni.
    max_msg = 128 * 1024 * 1024
    server = grpc.server(
        futures.ThreadPoolExecutor(max_workers=10),
        options=[
            ("grpc.max_send_message_length", max_msg),
            ("grpc.max_receive_message_length", max_msg),
        ],
    )
    pb2_grpc.add_NeuralCoreServiceServicer_to_server(NeuralCoreServicer(), server)
    server.add_insecure_port(f"[::]:{port}")
    server.start()
    logger.info("Neural Core gRPC server listening on port %d (max msg %d MB)", port, max_msg // (1024 * 1024))
    server.wait_for_termination()


if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO)
    serve()
