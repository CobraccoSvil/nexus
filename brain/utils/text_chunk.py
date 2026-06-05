"""Chunking testo con overlap: paritetico al Rust ``crates/mcp-core/src/rag/chunker.rs``.

Punto unico Python (regola L / ADR 0026). Prima esistevano 2 implementazioni
divergenti:
  - ``brain/embeddings/service.py::_chunk_text`` (split per linee, NO overlap)
  - ``brain/agents/context_offload.py::_chunk_text`` (sliding window char, overlap
    ma SENZA word-boundary)
Entrambe diverse dalla Rust ``rag::chunker::chunk_text`` (sliding window char
con overlap + smart trimming su whitespace), causando recall RAG inconsistente
tra ingest Python e query Rust.

L'algoritmo qui replica fedelmente la versione Rust. La parita' bit-per-bit e'
verificata da un golden test cross-language (``tests/fixtures/chunker_golden.json``,
caricato da pytest e da ``cargo test``).
"""
from __future__ import annotations

from typing import List


def chunk_text(text: str, chunk_size: int, overlap: int) -> List[str]:
    """Suddivide ``text`` in chunk. Garantisce:

    - ``len(chunk) <= chunk_size`` (in numero di caratteri Unicode, non bytes);
    - finestre consecutive condividono ``overlap`` caratteri;
    - mai chunk vuoti (eventuale ``trim()`` finale per i bordi).

    Paritetico a ``crates/mcp-core/src/rag/chunker.rs::chunk_text``.
    """
    if not text or chunk_size == 0:
        return []
    overlap = min(overlap, max(chunk_size - 1, 0))
    chars: List[str] = list(text)
    n = len(chars)
    if n <= chunk_size:
        return [text]

    step = chunk_size - overlap
    out: List[str] = []
    start = 0
    while start < n:
        end = min(start + chunk_size, n)
        # Cerca un boundary di whitespace vicino a `end` per non spezzare
        # parole, ma solo se l'aggiustamento e' piccolo (<= 50 char).
        if end < n:
            k = end
            min_k = max(end - 50, start + 1)
            while k > min_k and not chars[k - 1].isspace():
                k -= 1
            real_end = k if k > min_k else end
        else:
            real_end = end

        slice_str = "".join(chars[start:real_end]).strip()
        if slice_str:
            out.append(slice_str)
        if real_end >= n:
            break
        # Garantisce progresso di almeno `step` rispetto allo start CORRENTE.
        prev_start = start
        next_start = max(real_end - overlap, 0)
        if next_start < prev_start + step:
            next_start = prev_start + step
        if next_start >= n:
            break
        start = next_start
    return out
