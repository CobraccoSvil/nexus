"""Endpoint vision: /vision/describe e /vision/compare.

Provider/modello risolti via `nexus_purpose_model` (vision_describe / visual_compare).
Niente fallback hardcoded (regola G): se il purpose non e' configurato gli
endpoint ritornano 503.
"""
from __future__ import annotations

import logging

from fastapi import APIRouter, HTTPException
from pydantic import BaseModel

logger = logging.getLogger(__name__)

router = APIRouter()


class VisionDescribeRequest(BaseModel):
    """Body per POST /vision/describe.

    Usato dal tool agente nexus_describe_image_attachment (crates/mcp-core/
    src/agent_tools/vision_tools.rs). Provider/modello risolti via
    nexus_purpose_model.vision_describe (mig 0194). Niente fallback
    hardcoded: se il purpose non e configurato l endpoint ritorna 503.
    """
    image_base64: str
    mime_type: str
    question: str | None = None


_VISION_DEFAULT_PROMPT = (
    "Descrivi il contenuto visivo dell immagine in italiano. "
    "Se contiene testo leggibile riporta tutti i testi nella sezione OCR. "
    "Formato risposta esatto: DESCRIZIONE: ...\nOCR: ... "
    "(riporta sezione OCR vuota se non ce testo)."
)
# Limite hard sulla decoded payload: stesso default del tool agente (2 MB).
# Il limite finale e quello agente (Rust legge agent.attachment.image_max_bytes
# prima di chiamare); questa e rete safety.
_VISION_MAX_DECODED_BYTES = 2 * 1024 * 1024


def _parse_vision_response(text: str) -> tuple[str, str | None]:
    """Separa il payload DESCRIZIONE/OCR in (descrizione, ocr).

    Se il modello non rispetta il formato ritorna l intero testo come
    descrizione e ocr=None.
    """
    if not text:
        return "", None
    upper = text.upper()
    desc_idx = upper.find("DESCRIZIONE:")
    ocr_idx = upper.find("OCR:")
    if desc_idx == -1:
        return text.strip(), None
    desc_start = desc_idx + len("DESCRIZIONE:")
    if ocr_idx == -1 or ocr_idx < desc_idx:
        return text[desc_start:].strip(), None
    description = text[desc_start:ocr_idx].strip()
    ocr_text = text[ocr_idx + len("OCR:"):].strip()
    if not ocr_text:
        ocr_value: str | None = None
    else:
        ocr_value = ocr_text
    return description, ocr_value


def _data_uri(mime: str, image_bytes: bytes) -> str:
    """Costruisce un data URI ``data:<mime>;base64,<b64>`` da byte immagine.

    E' il formato accettato dal blocco ``image_url`` del gateway (mappato poi
    al dialetto del provider: openai_compat usa il data URI tale e quale,
    anthropic/google lo decodificano in source base64 / inline_data).
    """
    import base64 as _b64

    b64 = _b64.b64encode(image_bytes).decode("ascii")
    return f"data:{mime};base64,{b64}"


async def _gateway_vision_call(
    *,
    endpoint: str,
    provider_name: str,
    model: str,
    messages: list[dict],
) -> str:
    """Esegue la chiamata vision via GatewayProvider e ritorna il testo grezzo.

    Punto unico (regola L) per /vision/describe e /vision/compare: costruisce il
    model pinnato ``provider/model``, delega a ``generate_agent_turn`` (nessun
    tool) e mappa l'esito sullo stesso schema di errore degli endpoint:
      - 502 se il gateway/provider segnala errore (ProviderResult error);
      - 502 se la risposta e' vuota (nessun contenuto utile da parsare).
    Il routing/cooldown/privacy vivono nel gateway; qui niente SDK vendor.
    """
    from brain.providers.gateway_provider import GatewayProvider

    pinned_model = f"{provider_name}/{model}"
    try:
        result = await GatewayProvider().generate_agent_turn(
            model=pinned_model,
            messages=messages,
            tools=[],
            max_tokens=2048,
            system_text="",
        )
    except Exception as exc:  # noqa: BLE001
        logger.error("%s: chiamata gateway fallita: %s", endpoint, exc)
        raise HTTPException(
            status_code=502,
            detail=f"Provider {provider_name} vision via gateway fallito: {exc}",
        )

    meta = result.metadata or {}
    if meta.get("stop_reason") == "error" or meta.get("error"):
        err = meta.get("error") or "errore non specificato"
        logger.error("%s: gateway ha segnalato errore (%s/%s): %s", endpoint, provider_name, model, err)
        raise HTTPException(
            status_code=502,
            detail=f"Provider {provider_name} vision via gateway fallito: {err}",
        )

    return result.content or ""


@router.post("/vision/describe")
async def vision_describe(body: VisionDescribeRequest) -> dict[str, object]:
    """Descrive un immagine usando il modello configurato in
    nexus_purpose_model.vision_describe.

    La chiamata multimodale passa dal gateway LLM Rust (GatewayProvider) invece
    che dagli SDK vendor diretti: il brain costruisce i messaggi con un blocco
    ``image_url`` (data URI) e delega a ``generate_agent_turn`` con il model nel
    formato ``provider/model`` (pin del provider risolto dal purpose). Il
    gateway possiede routing/cooldown/privacy e mappa il blocco immagine al
    dialetto del provider. Gli SDK e ``_get_client`` restano per batch/catalog.

    Errori espliciti (no fallback nascosti):
      - 503 se purpose non configurato o mcp-core irraggiungibile;
      - 413 se la dimensione decoded supera 2 MB;
      - 400 se il base64 non e decodificabile;
      - 502 se il provider vision risponde con errore.
    """
    import base64 as _b64
    import time as _time

    from brain.router.service import _routing_client_singleton

    t0 = _time.perf_counter()

    # 1) Decoded payload + size guard.
    try:
        image_bytes = _b64.b64decode(body.image_base64, validate=True)
    except Exception as exc:
        raise HTTPException(status_code=400, detail=f"image_base64 non decodificabile: {exc}")
    if len(image_bytes) > _VISION_MAX_DECODED_BYTES:
        raise HTTPException(
            status_code=413,
            detail=(
                f"immagine troppo grande ({len(image_bytes)} byte), "
                f"limite {_VISION_MAX_DECODED_BYTES} byte"
            ),
        )
    mime = (body.mime_type or "application/octet-stream").strip().lower()
    if not mime.startswith("image/"):
        raise HTTPException(status_code=400, detail=f"mime_type non e image/*: {mime}")

    # 2) Risolvi provider/model via purpose. No fallback hardcoded.
    try:
        decision = _routing_client_singleton().purpose_model(purpose="vision_describe")
    except Exception as exc:
        logger.error("vision_describe: purpose_model lookup fallito: %s", exc)
        raise HTTPException(
            status_code=503,
            detail=f"nexus_purpose_model.vision_describe non risolvibile: {exc}",
        )
    provider_name = (decision.provider or "").strip()
    model = (decision.model or "").strip()
    if not provider_name or not model or provider_name.startswith("__"):
        logger.error(
            "vision_describe: purpose non configurato (provider=%r model=%r). Applica mig 0194.",
            provider_name, model,
        )
        raise HTTPException(
            status_code=503,
            detail=(
                "nexus_purpose_model.vision_describe non configurato. "
                "Applica db/migrations/0194_vision_describe_purpose.sql."
            ),
        )

    prompt_text = (body.question or "").strip() or _VISION_DEFAULT_PROMPT

    # 3) Esegui call multimodale via gateway (GatewayProvider). Il model e' nel
    #    formato "provider/model" -> il gateway pinna ESATTAMENTE quel provider.
    messages = [
        {
            "role": "user",
            "content": [
                {"type": "text", "text": prompt_text},
                {"type": "image_url", "image_url": {"url": _data_uri(mime, image_bytes)}},
            ],
        }
    ]
    text = await _gateway_vision_call(
        endpoint="vision_describe",
        provider_name=provider_name,
        model=model,
        messages=messages,
    )

    description, ocr_text = _parse_vision_response(text)
    elapsed_ms = int((_time.perf_counter() - t0) * 1000)
    logger.info(
        "vision_describe: provider=%s model=%s elapsed_ms=%d bytes=%d ocr=%s",
        provider_name, model, elapsed_ms, len(image_bytes), bool(ocr_text),
    )
    payload: dict[str, object] = {
        "description": description,
        "model_used": f"{provider_name}/{model}",
        "elapsed_ms": elapsed_ms,
    }
    if ocr_text:
        payload["ocr_text"] = ocr_text
    return payload


class VisualCompareRequest(BaseModel):
    """Body per POST /vision/compare.

    Usato dal tool agente nexus_visual_compare (crates/mcp-core/src/
    agent_tools/visual_compare.rs). Confronta lo screenshot dell app costruita
    (screenshot_base64) con il design di riferimento (reference_base64) usando
    il modello vision risolto via nexus_purpose_model.visual_compare (mig 0214).
    Niente fallback hardcoded: se il purpose non e configurato l endpoint
    ritorna 503.
    """
    screenshot_base64: str
    screenshot_mime: str
    reference_base64: str
    reference_mime: str


_VISUAL_COMPARE_PROMPT = (
    "Sei un revisore di design UI. Ti fornisco DUE immagini: la prima e lo "
    "SCREENSHOT dell app realmente costruita, la seconda e il DESIGN DI "
    "RIFERIMENTO (mockup Figma) che l app deve replicare. Confronta lo "
    "screenshot col riferimento ed elenca SOLO gli scostamenti di design "
    "ATTUABILI. Considera: palette e colori, tipografia (font, pesi, "
    "dimensioni), spaziature e margini, layout e posizionamento, componenti "
    "mancanti o in piu rispetto al riferimento. Stima la similarita visiva "
    "complessiva da 0 a 100. Rispondi ESCLUSIVAMENTE con un oggetto JSON "
    "valido, senza testo prima o dopo, in questo formato esatto: "
    '{"similarity_score": <intero 0-100>, "differences": [ '
    '{"category": "colore|tipografia|layout|spaziatura|componente", '
    '"severity": "alta|media|bassa", "description": "<descrizione in '
    'italiano>", "suggested_fix": "<correzione concreta in italiano>"} ] }. '
    "Le descrizioni e i suggested_fix devono essere in italiano e azionabili "
    "(es. classi Tailwind, valori CSS, spostamenti di componenti)."
)


def _parse_visual_compare_response(text: str) -> dict[str, object]:
    """Estrae l oggetto JSON {similarity_score, differences} dalla risposta del
    modello. Tollerante: se il modello avvolge il JSON in markdown o testo,
    isola il primo oggetto graffe-bilanciato. Se non e parsabile ritorna una
    struttura di errore strutturata (mai eccezione verso il chiamante)."""
    import json as _json

    if not text:
        return {"similarity_score": None, "differences": [], "parse_error": "risposta vuota"}
    raw = text.strip()
    # Rimuovi eventuali fence markdown.
    if raw.startswith("```"):
        first_nl = raw.find("\n")
        if first_nl != -1:
            raw = raw[first_nl + 1:]
        if raw.rstrip().endswith("```"):
            raw = raw.rstrip()[:-3]
    raw = raw.strip()
    # Isola il primo oggetto JSON graffe-bilanciato.
    start = raw.find("{")
    if start != -1:
        depth = 0
        in_str = False
        prev_escape = False
        for i in range(start, len(raw)):
            ch = raw[i]
            if ch == '"' and not prev_escape:
                in_str = not in_str
            elif not in_str and ch == "{":
                depth += 1
            elif not in_str and ch == "}":
                depth -= 1
                if depth == 0:
                    raw = raw[start:i + 1]
                    break
            prev_escape = ch == "\\" and not prev_escape
    try:
        parsed = _json.loads(raw)
    except Exception as exc:  # noqa: BLE001 - degradazione strutturata, no raise
        return {
            "similarity_score": None,
            "differences": [],
            "parse_error": f"risposta vision non e JSON valido: {exc}",
        }
    if not isinstance(parsed, dict):
        return {"similarity_score": None, "differences": [], "parse_error": "JSON non e un oggetto"}
    score = parsed.get("similarity_score")
    diffs = parsed.get("differences")
    if not isinstance(diffs, list):
        diffs = []
    return {"similarity_score": score, "differences": diffs}


@router.post("/vision/compare")
async def vision_compare(body: VisualCompareRequest) -> dict[str, object]:
    """Confronta lo screenshot dell app costruita col design di riferimento
    usando il modello configurato in nexus_purpose_model.visual_compare.

    Errori espliciti (no fallback nascosti), coerenti con /vision/describe:
      - 503 se purpose non configurato o mcp-core irraggiungibile;
      - 413 se una delle immagini decoded supera 2 MB;
      - 400 se un base64 non e decodificabile o il mime non e image/*;
      - 501 se il provider configurato non e supportato;
      - 502 se il provider vision risponde con errore.
    """
    import base64 as _b64
    import time as _time

    from brain.router.service import _routing_client_singleton

    t0 = _time.perf_counter()

    def _decode(label: str, b64: str, mime: str) -> tuple[bytes, str]:
        try:
            data = _b64.b64decode(b64, validate=True)
        except Exception as exc:
            raise HTTPException(status_code=400, detail=f"{label}_base64 non decodificabile: {exc}")
        if len(data) > _VISION_MAX_DECODED_BYTES:
            raise HTTPException(
                status_code=413,
                detail=f"{label} troppo grande ({len(data)} byte), limite {_VISION_MAX_DECODED_BYTES} byte",
            )
        m = (mime or "application/octet-stream").strip().lower()
        if not m.startswith("image/"):
            raise HTTPException(status_code=400, detail=f"{label}_mime non e image/*: {m}")
        return data, m

    screenshot_bytes, screenshot_mime = _decode("screenshot", body.screenshot_base64, body.screenshot_mime)
    reference_bytes, reference_mime = _decode("reference", body.reference_base64, body.reference_mime)

    # Risolvi provider/model via purpose. No fallback hardcoded.
    try:
        decision = _routing_client_singleton().purpose_model(purpose="visual_compare")
    except Exception as exc:
        logger.error("vision_compare: purpose_model lookup fallito: %s", exc)
        raise HTTPException(
            status_code=503,
            detail=f"nexus_purpose_model.visual_compare non risolvibile: {exc}",
        )
    provider_name = (decision.provider or "").strip()
    model = (decision.model or "").strip()
    if not provider_name or not model or provider_name.startswith("__"):
        logger.error(
            "vision_compare: purpose non configurato (provider=%r model=%r). Applica mig 0214.",
            provider_name, model,
        )
        raise HTTPException(
            status_code=503,
            detail=(
                "nexus_purpose_model.visual_compare non configurato. "
                "Applica db/migrations/0214_visual_compare_settings.sql."
            ),
        )

    # Call multimodale via gateway: prompt + DUE immagini (screenshot poi
    # reference, stesso ordine atteso dal prompt). Il model "provider/model"
    # pinna il provider sul gateway.
    messages = [
        {
            "role": "user",
            "content": [
                {"type": "text", "text": _VISUAL_COMPARE_PROMPT},
                {"type": "image_url", "image_url": {"url": _data_uri(screenshot_mime, screenshot_bytes)}},
                {"type": "image_url", "image_url": {"url": _data_uri(reference_mime, reference_bytes)}},
            ],
        }
    ]
    text = await _gateway_vision_call(
        endpoint="vision_compare",
        provider_name=provider_name,
        model=model,
        messages=messages,
    )

    parsed = _parse_visual_compare_response(text)
    elapsed_ms = int((_time.perf_counter() - t0) * 1000)
    logger.info(
        "vision_compare: provider=%s model=%s elapsed_ms=%d shot_bytes=%d ref_bytes=%d score=%s",
        provider_name, model, elapsed_ms, len(screenshot_bytes), len(reference_bytes),
        parsed.get("similarity_score"),
    )
    payload: dict[str, object] = {
        "similarity_score": parsed.get("similarity_score"),
        "differences": parsed.get("differences", []),
        "model_used": f"{provider_name}/{model}",
        "elapsed_ms": elapsed_ms,
    }
    if parsed.get("parse_error"):
        payload["parse_error"] = parsed["parse_error"]
    return payload
