#!/bin/bash
# Pacchettizza l'estensione IDEAI Browser Bridge in .zip e .crx.
#
# Uso:
#   ./pack.sh                # produce dist/*.zip e (se Chrome/Chromium presente) dist/*.crx
#   ./pack.sh --zip-only     # solo zip
#
# La chiave privata persistente vive in dist/key.pem (gitignore consigliato).
# Stessa chiave => stesso extension ID stabile fra build, requisito per
# whitelist/policy enterprise e per gli aggiornamenti.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
DIST="${ROOT}/dist"
SRC="${ROOT}"

ZIP_ONLY=false
for arg in "$@"; do
    [ "$arg" = "--zip-only" ] && ZIP_ONLY=true
done

mkdir -p "$DIST"

VERSION=$(grep -m1 '"version"' "${SRC}/manifest.json" | sed -E 's/.*"version"[^"]*"([^"]+)".*/\1/')
NAME="browser-bridge-extension-${VERSION}"

# ---- ZIP ---------------------------------------------------------------
echo "==> creo ${DIST}/${NAME}.zip"
rm -f "${DIST}/${NAME}.zip"
if command -v zip >/dev/null 2>&1; then
    (cd "$SRC" && zip -r "${DIST}/${NAME}.zip" \
        manifest.json background.js popup.html popup.js >/dev/null)
else
    # fallback: Python stdlib (sempre presente su WSL Ubuntu)
    python3 - "$SRC" "${DIST}/${NAME}.zip" <<'PY'
import os, sys, zipfile
src, out = sys.argv[1], sys.argv[2]
files = ["manifest.json", "background.js", "popup.html", "popup.js"]
with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
    for f in files:
        z.write(os.path.join(src, f), arcname=f)
PY
fi
echo "    ok ($(du -h "${DIST}/${NAME}.zip" | cut -f1))"

if $ZIP_ONLY; then
    echo "==> --zip-only attivo, salto generazione .crx"
    exit 0
fi

# ---- CRX ---------------------------------------------------------------
CHROME=""
for cand in google-chrome chromium chromium-browser /opt/google/chrome/chrome; do
    if command -v "$cand" >/dev/null 2>&1; then CHROME="$cand"; break; fi
done

if [ -z "$CHROME" ]; then
    echo "==> Chrome/Chromium non trovato in WSL: salto .crx"
    echo "    -> installa con: sudo apt install chromium-browser"
    echo "    -> oppure usa solo lo .zip ${DIST}/${NAME}.zip"
    exit 0
fi

# Cartella sorgente "pulita" per --pack-extension (deve contenere solo i file estensione).
PACKDIR="${DIST}/_pack-${VERSION}"
rm -rf "$PACKDIR"
mkdir -p "$PACKDIR"
cp "${SRC}/manifest.json" "${SRC}/background.js" "${SRC}/popup.html" "${SRC}/popup.js" "$PACKDIR"/

KEY="${DIST}/key.pem"
if [ -f "$KEY" ]; then
    echo "==> riuso chiave esistente: ${KEY}"
    "$CHROME" --pack-extension="$PACKDIR" --pack-extension-key="$KEY" --no-sandbox >/dev/null 2>&1 || true
else
    echo "==> genero nuova chiave: ${KEY}"
    "$CHROME" --pack-extension="$PACKDIR" --no-sandbox >/dev/null 2>&1 || true
    if [ -f "${DIST}/_pack-${VERSION}.pem" ]; then
        mv "${DIST}/_pack-${VERSION}.pem" "$KEY"
        chmod 600 "$KEY"
    fi
fi

if [ -f "${DIST}/_pack-${VERSION}.crx" ]; then
    mv "${DIST}/_pack-${VERSION}.crx" "${DIST}/${NAME}.crx"
    rm -rf "$PACKDIR"
    echo "    ok ${DIST}/${NAME}.crx ($(du -h "${DIST}/${NAME}.crx" | cut -f1))"
else
    echo "==> ERRORE: Chrome non ha prodotto .crx (controlla output sopra)"
    exit 1
fi

echo ""
echo "Output:"
echo "  ZIP : ${DIST}/${NAME}.zip   (per Chrome Web Store o load unpacked dopo unzip)"
echo "  CRX : ${DIST}/${NAME}.crx   (drag&drop su chrome://extensions con Dev Mode ON)"
echo "  KEY : ${KEY}                 (NON committare: tieni privata, garantisce ID stabile)"
