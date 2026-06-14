"""Google Gemini Batch API client for deep file review.

ECCEZIONE SDK LEGITTIMA (regole G/L, migrazione gateway 2026-06-14): tutte le
chiamate LLM del brain passano dal gateway Rust, TRANNE questo batch Vertex.
Motivo: il flusso usa upload/download di file via Vertex Files API
(files.upload -> batches.create -> batches.get -> files.download), non riducibile
a REST pulita come il batch Anthropic. Il gateway ritorna 501 di proposito
(crates/nexus-gateway/src/batch.rs). Migrarlo richiederebbe ~600-800 righe Rust
(streaming multipart + resumable upload) con rischio alto: rimane in Python come
caso "SDK piu' avanzato del REST puro", finche' non esiste una soluzione Rust
robusta per il file-transfer Vertex.
"""

from google import genai


def _resolve_google_batch_model() -> str:
    """Risolve il modello Google batch da nexus_purpose_model (purpose='google_batch')."""
    try:
        from brain.router.service import _routing_client_singleton
        decision = _routing_client_singleton().purpose_model(purpose="google_batch")
        return decision.model
    except Exception as e:
        raise RuntimeError(
            f"nexus_purpose_model purpose='google_batch' non configurato: {e}"
        ) from e


class GoogleBatchClient:
    def __init__(self, api_key: str, model: str | None = None):
        self._client = genai.Client(api_key=api_key)
        self._model = model or _resolve_google_batch_model()

    async def analyze_files_batch(
        self,
        files: list[dict],  # [{"path": str, "content": str}]
        analysis_prompt: str = (
            'Analizza questo file per problemi di qualità, sicurezza, performance e best practice. '
            'Rispondi in JSON: {"issues": [{"line": N, "severity": "high|medium|low", '
            '"category": str, "message": str, "suggestion": str}]}'
        )
    ) -> str:
        """Submit a batch job for file analysis. Returns job_name."""
        import json
        import tempfile
        import os

        requests = []
        for f in files:
            requests.append({
                "key": f["path"],
                "request": {
                    "contents": [{
                        "role": "user",
                        "parts": [{"text": f"{analysis_prompt}\n\nFile: {f['path']}\n\n```\n{f['content'][:8000]}\n```"}]
                    }]
                }
            })

        # Write JSONL
        with tempfile.NamedTemporaryFile(mode='w', suffix='.jsonl', delete=False) as tmp:
            for req in requests:
                tmp.write(json.dumps(req) + '\n')
            tmp_path = tmp.name

        try:
            # Upload file
            with open(tmp_path, 'rb') as f:
                uploaded_file = self._client.files.upload(
                    file=f,
                    config={"mime_type": "application/jsonl"}
                )

            # Create batch job
            batch_job = self._client.batches.create(
                model=self._model,
                src=uploaded_file.name,
                config={"display_name": f"ideai-deep-review-{len(files)}files"},
            )
            return batch_job.name
        finally:
            os.unlink(tmp_path)

    def get_job_status(self, job_name: str) -> dict:
        """Returns {"state": str, "completed": int, "total": int}"""
        job = self._client.batches.get(name=job_name)
        return {
            "state": job.state.name,
            "completed": getattr(job, "completed_count", 0),
            "total": getattr(job, "total_count", 0),
            "job_name": job_name,
        }

    def get_results(self, job_name: str) -> list[dict]:
        """Download and parse batch results. Returns list of {path, issues}."""
        import json
        job = self._client.batches.get(name=job_name)
        if job.state.name != "JOB_STATE_SUCCEEDED":
            return []

        dest_file = job.dest.file_name if hasattr(job, 'dest') and job.dest else None
        if not dest_file:
            return []

        raw = self._client.files.download(file=dest_file)
        content = raw.decode("utf-8") if isinstance(raw, bytes) else raw

        results = []
        for line in content.splitlines():
            if not line.strip():
                continue
            try:
                item = json.loads(line)
                path = item.get("key", "")
                response_text = ""
                try:
                    response_text = item["response"]["candidates"][0]["content"]["parts"][0]["text"]
                except Exception:
                    pass
                # Parse JSON from response
                issues = []
                try:
                    # Find JSON block in response
                    start = response_text.find('{')
                    end = response_text.rfind('}') + 1
                    if start >= 0 and end > start:
                        parsed = json.loads(response_text[start:end])
                        issues = parsed.get("issues", [])
                except Exception:
                    pass
                results.append({"path": path, "issues": issues})
            except Exception:
                continue
        return results
