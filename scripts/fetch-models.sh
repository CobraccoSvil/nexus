#!/usr/bin/env bash
# fetch-models.sh — scarica i modelli ML non versionati nel repo (troppo grandi
# per git). Idempotente: scarica solo i file mancanti o di dimensione sospetta.
#
# Modelli gestiti:
#   - all-MiniLM-L6-v2 (ONNX, 384d) per OnnxMiniLmEmbedder (semantic tool search).
#     Richiesto dalla feature `onnx` di mcp-core; senza, fallback a HashEmbedder.
#
# Uso: ./scripts/fetch-models.sh   (eseguito anche da deploy-local.sh prima del build Rust)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MINILM_DIR="${ROOT}/models/minilm"

# Sorgente: Xenova/all-MiniLM-L6-v2 (export ONNX di sentence-transformers).
MODEL_URL="https://huggingface.co/Xenova/all-MiniLM-L6-v2/resolve/main/onnx/model.onnx"
TOKENIZER_URL="https://huggingface.co/Xenova/all-MiniLM-L6-v2/resolve/main/tokenizer.json"

# Soglie minime di sanita' (byte): un download troncato e' piu' piccolo.
MODEL_MIN_BYTES=50000000     # ~90MB attesi
TOKENIZER_MIN_BYTES=100000   # ~700KB attesi

file_ok() {
  local path="$1" min="$2"
  [ -f "$path" ] || return 1
  local size
  size=$(wc -c < "$path")
  [ "$size" -ge "$min" ]
}

mkdir -p "$MINILM_DIR"

if file_ok "${MINILM_DIR}/model.onnx" "$MODEL_MIN_BYTES"; then
  echo "[fetch-models] model.onnx gia' presente, skip."
else
  echo "[fetch-models] scarico model.onnx (~90MB)..."
  curl -fSL --retry 3 -o "${MINILM_DIR}/model.onnx" "$MODEL_URL"
  echo "[fetch-models] model.onnx OK ($(wc -c < "${MINILM_DIR}/model.onnx") byte)"
fi

if file_ok "${MINILM_DIR}/tokenizer.json" "$TOKENIZER_MIN_BYTES"; then
  echo "[fetch-models] tokenizer.json gia' presente, skip."
else
  echo "[fetch-models] scarico tokenizer.json..."
  curl -fSL --retry 3 -o "${MINILM_DIR}/tokenizer.json" "$TOKENIZER_URL"
  echo "[fetch-models] tokenizer.json OK ($(wc -c < "${MINILM_DIR}/tokenizer.json") byte)"
fi

echo "[fetch-models] completato. Modelli in ${MINILM_DIR}"
