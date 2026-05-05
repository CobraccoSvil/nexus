#!/usr/bin/env bash
# Scarica i modelli ONNX necessari per NexusBridge.
#
# Modelli scaricati:
#   - all-MiniLM-L6-v2 (384-d) per embedding semantico del Q-Learning router
#
# Uso:
#   ./scripts/download_models.sh [--dir /percorso/personalizzato]
#
# Default output: models/minilm/
#
# Richiede: curl o wget disponibile nel PATH.

set -euo pipefail

# ---------------------------------------------------------------------------
# Configurazione
# ---------------------------------------------------------------------------
MODELS_DIR="${NEXUS_MODELS_DIR:-models/minilm}"

HF_BASE="https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main"

# File richiesti
MODEL_FILE="onnx/model.onnx"
TOKENIZER_FILE="tokenizer.json"
# Special tokens map e config (utili per debug/ispezione)
CONFIG_FILE="config.json"

# ---------------------------------------------------------------------------
# Parsing argomenti
# ---------------------------------------------------------------------------
while [[ $# -gt 0 ]]; do
    case "$1" in
        --dir)
            MODELS_DIR="$2"
            shift 2
            ;;
        --help|-h)
            echo "Usage: $0 [--dir /path/to/output]"
            exit 0
            ;;
        *)
            echo "Unknown argument: $1"
            exit 1
            ;;
    esac
done

# ---------------------------------------------------------------------------
# Utility
# ---------------------------------------------------------------------------
download() {
    local url="$1"
    local dest="$2"

    if [ -f "$dest" ]; then
        echo "  ✓ Already exists: $dest (skipping)"
        return
    fi

    echo "  ↓ Downloading: $url"
    if command -v curl &>/dev/null; then
        curl -L --progress-bar -o "$dest" "$url"
    elif command -v wget &>/dev/null; then
        wget -q --show-progress -O "$dest" "$url"
    else
        echo "ERROR: neither curl nor wget found in PATH"
        exit 1
    fi
    echo "  ✓ Saved to: $dest"
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
echo "=== NEXUS Model Downloader ==="
echo "Output dir: $MODELS_DIR"
echo ""

mkdir -p "$MODELS_DIR"

# Modello ONNX (~90 MB)
download "${HF_BASE}/${MODEL_FILE}" "${MODELS_DIR}/model.onnx"

# Tokenizer WordPiece (~600 KB)
download "${HF_BASE}/${TOKENIZER_FILE}" "${MODELS_DIR}/tokenizer.json"

# Config opzionale (per debug)
download "${HF_BASE}/${CONFIG_FILE}" "${MODELS_DIR}/config.json" || true

echo ""
echo "=== Download completato ==="
echo ""
echo "Per abilitare l'embedder ONNX, aggiungi al .env:"
echo "  NEXUS_MINILM_MODEL=${MODELS_DIR}/model.onnx"
echo "  NEXUS_MINILM_TOKENIZER=${MODELS_DIR}/tokenizer.json"
echo ""
echo "oppure esegui il processo dalla root del workspace (models/ è il default)."
