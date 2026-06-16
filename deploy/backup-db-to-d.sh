#!/usr/bin/env bash
# Backup del database Nexus (Postgres in container ideai-postgres-nexus-1).
# Tre destinazioni, in cascata:
#   1. WSL locale: /home/administrator/ideai/backups/*.dump
#   2. D:\Backups\Nexus\ (host Windows, accessibile da WSL via /mnt/d)
#   3. Google Drive (off-site) via rclone, su <remote>:Nexus/Backups
#
# Uso:
#   bash deploy/backup-db-to-d.sh             # nuovo dump + sync D: + upload Drive
#   bash deploy/backup-db-to-d.sh --sync      # solo sync/upload (no nuovo dump)
#   bash deploy/backup-db-to-d.sh --keep N    # tiene N backup piu recenti (default 14)
#   bash deploy/backup-db-to-d.sh --no-gdrive # salta l'upload su Google Drive
#
# Copia off-site su Google Drive (rclone):
#   - Prerequisito una-tantum: installare rclone e configurare un remote Google
#     Drive con `rclone config` (tipo "drive"); nome di default atteso: gdrive.
#   - Se rclone non e' installato o il remote non e' configurato, lo step Drive
#     viene SALTATO con un avviso (il backup locale e su D: riesce comunque).
#   - Override via env: RCLONE_REMOTE (default gdrive), GDRIVE_BACKUP_PATH
#     (default Nexus/Backups), GDRIVE_KEEP_COUNT (default = --keep), RCLONE_BIN.
#
# I .dump NON vengono mai committati in git: vivono solo in WSL
# (/home/administrator/ideai/backups), su D:\Backups\Nexus\ e su Google Drive.
# Il .gitignore esclude esplicitamente backups/*.dump.

set -eu

# ── Configurazione ──────────────────────────────────────────────────────────
DB_CONTAINER="${DB_CONTAINER:-ideai-postgres-nexus-1}"
DB_NAME="${DB_NAME:-nexus}"
DB_USER="${DB_USER:-nexus}"
WSL_BACKUP_DIR="${WSL_BACKUP_DIR:-/home/administrator/ideai/backups}"
WIN_BACKUP_DIR="${WIN_BACKUP_DIR:-/mnt/d/Backups/Nexus}"
KEEP_COUNT="${KEEP_COUNT:-14}"

# ── Copia off-site Google Drive (rclone) ────────────────────────────────────
# Risolvi il binario rclone: override esplicito > PATH > ~/.local/bin
# (installazione user-space senza sudo). Cosi' lo step Drive funziona anche
# da esecuzioni schedulate/headless che non caricano il profilo della shell.
if [ -z "${RCLONE_BIN:-}" ]; then
    if command -v rclone >/dev/null 2>&1; then
        RCLONE_BIN="rclone"
    elif [ -x "$HOME/.local/bin/rclone" ]; then
        RCLONE_BIN="$HOME/.local/bin/rclone"
    else
        RCLONE_BIN="rclone"  # assente: lo step Drive verra' saltato con avviso
    fi
fi
RCLONE_REMOTE="${RCLONE_REMOTE:-gdrive}"
GDRIVE_BACKUP_PATH="${GDRIVE_BACKUP_PATH:-Nexus/Backups}"
GDRIVE_KEEP_COUNT="${GDRIVE_KEEP_COUNT:-$KEEP_COUNT}"
DO_GDRIVE=true

# ── Parsing argomenti ───────────────────────────────────────────────────────
DO_DUMP=true
while [ $# -gt 0 ]; do
    case "$1" in
        --sync)
            DO_DUMP=false
            shift
            ;;
        --keep)
            KEEP_COUNT="$2"
            shift 2
            ;;
        --no-gdrive)
            DO_GDRIVE=false
            shift
            ;;
        --help|-h)
            sed -n '2,/^set -eu/p' "$0" | sed 's/^# \?//'
            exit 0
            ;;
        *)
            echo "Argomento sconosciuto: $1" >&2
            exit 2
            ;;
    esac
done

# ── Verifica preconditions ──────────────────────────────────────────────────
if [ ! -d "/mnt/d" ]; then
    echo "ERRORE: /mnt/d non accessibile. WSL deve avere il disco D: montato." >&2
    exit 1
fi

mkdir -p "$WSL_BACKUP_DIR" "$WIN_BACKUP_DIR"

if ! docker inspect "$DB_CONTAINER" >/dev/null 2>&1; then
    echo "ERRORE: container '$DB_CONTAINER' non trovato." >&2
    exit 1
fi

container_status=$(docker inspect "$DB_CONTAINER" --format "{{.State.Status}}")
if [ "$container_status" != "running" ]; then
    echo "ERRORE: container '$DB_CONTAINER' non e' in running (status=$container_status)." >&2
    exit 1
fi

# ── Step 1: pg_dump (se non --sync) ─────────────────────────────────────────
if $DO_DUMP; then
    TS=$(date +%Y%m%d_%H%M%S)
    BACKUP_FILE="$WSL_BACKUP_DIR/nexus-db-backup-$TS.dump"

    echo "==> pg_dump → $BACKUP_FILE"
    docker exec "$DB_CONTAINER" pg_dump -U "$DB_USER" -d "$DB_NAME" -Fc -Z 9 > "$BACKUP_FILE"

    # Verifica integrita: pg_restore --list deve produrre almeno 1 riga
    list_lines=$(docker exec -i "$DB_CONTAINER" pg_restore --list < "$BACKUP_FILE" 2>/dev/null | wc -l)
    if [ "$list_lines" -lt 10 ]; then
        echo "ERRORE: backup '$BACKUP_FILE' sembra corrotto (solo $list_lines righe nel listing)." >&2
        rm -f "$BACKUP_FILE"
        exit 1
    fi

    backup_size=$(du -h "$BACKUP_FILE" | cut -f1)
    echo "  size: $backup_size — manifest: $list_lines oggetti — OK"
fi

# ── Step 2: sync WSL → D:\Backups\Nexus ─────────────────────────────────────
echo
echo "==> Sync $WSL_BACKUP_DIR → $WIN_BACKUP_DIR"
copied=0
for f in "$WSL_BACKUP_DIR"/*.dump "$WSL_BACKUP_DIR"/postgres/*.sql.gz; do
    [ -f "$f" ] || continue
    name=$(basename "$f")
    if [ -f "$WIN_BACKUP_DIR/$name" ]; then
        src_size=$(stat -c '%s' "$f")
        dst_size=$(stat -c '%s' "$WIN_BACKUP_DIR/$name")
        if [ "$src_size" = "$dst_size" ]; then
            continue
        fi
    fi
    cp "$f" "$WIN_BACKUP_DIR/$name"
    copied=$((copied + 1))
    echo "  copiato: $name"
done
if [ "$copied" -eq 0 ]; then
    echo "  (nessun nuovo backup da copiare)"
fi

# ── Step 3: rotation su D: (tiene gli ultimi N) ─────────────────────────────
echo
echo "==> Rotation: tieni gli ultimi $KEEP_COUNT backup su D:"
total=$(ls -1 "$WIN_BACKUP_DIR"/*.dump 2>/dev/null | wc -l)
if [ "$total" -gt "$KEEP_COUNT" ]; then
    to_delete=$((total - KEEP_COUNT))
    ls -1t "$WIN_BACKUP_DIR"/*.dump | tail -n "$to_delete" | while read old; do
        echo "  rimuovo (vecchio): $(basename "$old")"
        rm -f "$old"
    done
else
    echo "  ($total backup, sotto la soglia di $KEEP_COUNT — nessuna rotation)"
fi

# ── Step 4: upload off-site su Google Drive (rclone) ────────────────────────
echo
if ! $DO_GDRIVE; then
    echo "==> Google Drive: upload saltato (--no-gdrive)"
elif ! command -v "$RCLONE_BIN" >/dev/null 2>&1; then
    echo "==> Google Drive: SALTATO — rclone non installato." >&2
    echo "    Installa rclone e configura il remote con 'rclone config' (tipo: drive)." >&2
elif ! "$RCLONE_BIN" listremotes 2>/dev/null | grep -qx "${RCLONE_REMOTE}:"; then
    echo "==> Google Drive: SALTATO — remote '${RCLONE_REMOTE}:' non configurato." >&2
    echo "    Esegui 'rclone config' e crea un remote Google Drive chiamato '${RCLONE_REMOTE}'," >&2
    echo "    oppure imposta RCLONE_REMOTE col nome del tuo remote." >&2
else
    REMOTE_DEST="${RCLONE_REMOTE}:${GDRIVE_BACKUP_PATH}"
    echo "==> Upload $WSL_BACKUP_DIR/*.dump → $REMOTE_DEST"
    # --include "*.dump" + --max-depth 1: carica solo i .dump del livello
    # principale (i backup di QUESTO script), non la sottocartella postgres/
    # (dump di scripts/db-backup.sh) ne' i .sql.gz. Coerente con la rotation
    # remota qui sotto, che opera sul primo livello. rclone copy salta i file
    # gia' presenti e identici (checksum).
    if "$RCLONE_BIN" copy "$WSL_BACKUP_DIR" "$REMOTE_DEST" --include "*.dump" --max-depth 1 \
        2>/tmp/rclone-backup.err; then
        echo "  upload OK"
        # Rotation remota: tieni gli ultimi GDRIVE_KEEP_COUNT .dump. I nomi
        # contengono il timestamp (nexus-db-backup-YYYYMMDD_HHMMSS.dump), quindi
        # l'ordine lessicografico coincide con l'ordine cronologico.
        if [[ "$GDRIVE_KEEP_COUNT" =~ ^[0-9]+$ ]] && [ "$GDRIVE_KEEP_COUNT" -gt 0 ]; then
            mapfile -t remote_dumps < <("$RCLONE_BIN" lsf "$REMOTE_DEST" --include "*.dump" 2>/dev/null | sort)
            rtotal=${#remote_dumps[@]}
            if [ "$rtotal" -gt "$GDRIVE_KEEP_COUNT" ]; then
                rdel=$((rtotal - GDRIVE_KEEP_COUNT))
                echo "  rotation Drive: rimuovo $rdel backup piu vecchi (tengo $GDRIVE_KEEP_COUNT)"
                for ((i=0; i<rdel; i++)); do
                    name="${remote_dumps[$i]}"
                    [ -n "$name" ] || continue
                    if "$RCLONE_BIN" deletefile "$REMOTE_DEST/$name" 2>/dev/null; then
                        echo "    rimosso (Drive): $name"
                    fi
                done
            else
                echo "  ($rtotal backup su Drive, sotto la soglia di $GDRIVE_KEEP_COUNT — nessuna rotation)"
            fi
        fi
    else
        echo "==> Google Drive: upload FALLITO (vedi /tmp/rclone-backup.err) — proseguo." >&2
        sed 's/^/    /' /tmp/rclone-backup.err >&2 || true
    fi
fi

# ── Riepilogo ───────────────────────────────────────────────────────────────
echo
echo "==> Stato finale $WIN_BACKUP_DIR"
ls -lht "$WIN_BACKUP_DIR" | head -8
echo
echo "Totale: $(du -sh "$WIN_BACKUP_DIR" | cut -f1)"
echo "Spazio libero D:  $(df -h "$WIN_BACKUP_DIR" | awk 'NR==2 {print $4}')"

if $DO_GDRIVE && command -v "$RCLONE_BIN" >/dev/null 2>&1 \
   && "$RCLONE_BIN" listremotes 2>/dev/null | grep -qx "${RCLONE_REMOTE}:"; then
    echo
    echo "==> Google Drive ($RCLONE_REMOTE:$GDRIVE_BACKUP_PATH): ultimi backup"
    "$RCLONE_BIN" lsf "${RCLONE_REMOTE}:${GDRIVE_BACKUP_PATH}" --include "*.dump" 2>/dev/null \
        | sort | tail -8 | sed 's/^/  /'
fi
