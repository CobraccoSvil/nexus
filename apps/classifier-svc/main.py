"""
Nexus Classifier Service — Presidio PII detection.
Espone REST sulla porta 8002.
"""

from fastapi import FastAPI, HTTPException
from pydantic import BaseModel
from typing import Optional
import logging

logger = logging.getLogger("nexus-classifier")

app = FastAPI(title="Nexus Classifier Service", version="0.3.0")

# Presidio lazy init — se non disponibile, il servizio parte comunque in stub mode
_analyzer = None

def get_analyzer():
    global _analyzer
    if _analyzer is not None:
        return _analyzer
    try:
        from presidio_analyzer import AnalyzerEngine, RecognizerRegistry
        from presidio_analyzer.nlp_engine import NlpEngineProvider

        # Setup con supporto italiano
        provider = NlpEngineProvider(nlp_configuration={
            "nlp_engine_name": "spacy",
            "models": [
                {"lang_code": "it", "model_name": "it_core_news_sm"},
                {"lang_code": "en", "model_name": "en_core_web_sm"},
            ],
        })
        nlp_engine = provider.create_engine()
        registry = RecognizerRegistry()
        registry.load_predefined_recognizers()

        _analyzer = AnalyzerEngine(
            nlp_engine=nlp_engine,
            registry=registry,
            supported_languages=["it", "en"],
        )
        logger.info("Presidio AnalyzerEngine inizializzato")
    except Exception as e:
        logger.warning(f"Presidio non disponibile ({e}), stub mode attivo")
        _analyzer = None
    return _analyzer


class AnalyzeRequest(BaseModel):
    text: str
    language: str = "it"
    score_threshold: float = 0.5


class Entity(BaseModel):
    entity_type: str
    start: int
    end: int
    score: float


@app.get("/health")
def health():
    analyzer = get_analyzer()
    return {
        "status": "ok",
        "presidio": "ready" if analyzer is not None else "stub",
    }


@app.post("/analyze", response_model=list[Entity])
def analyze(req: AnalyzeRequest):
    analyzer = get_analyzer()
    if analyzer is None:
        # Stub mode: nessuna entity rilevata
        return []

    try:
        results = analyzer.analyze(
            text=req.text,
            language=req.language,
            score_threshold=req.score_threshold,
        )
        return [
            Entity(
                entity_type=r.entity_type,
                start=r.start,
                end=r.end,
                score=r.score,
            )
            for r in results
        ]
    except Exception as e:
        logger.error(f"Errore analisi Presidio: {e}")
        raise HTTPException(status_code=500, detail=str(e))
