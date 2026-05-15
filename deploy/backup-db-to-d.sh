#!/usr/bin/env bash
# Backup del database Nexus (Postgres in container ideai-postgres-nexus-1)
# su D:\Backups\Nexus\ (filesystem host Windows, accessibile da WSL via /mnt/d).
#
# Uso:
#   bash deploy/backup-db-to-d.sh           # crea un nuovo backup e syncronizza
#   bash deploy/backup-db-to-d.sh --sync    # solo sync da WSL a D: (no nuovo dump)
#   bash deploy/backup-db-to-d.sh --keep N  # tiene N backup piu recenti su D: (default 14)
#
# I .dump NON vengono mai committati in git: vivono solo in WSL
# (/home/administrator/ideai/backups) e su D:\Backups\Nexus\.
# Il .gitignore esclude esplicitamente backups/*.dump.

set -eu

# ── Configurazione ──────────────────────────────────────────────────────────
DB_CONTAINER="${DB_CONTAINER:-ideai-postgres-nexus-1}"
DB_NAME="${DB_NAME:-nexus}"
DB_USER="${DB_USER:-nexus}"
WSL_BACKUP_DIR="${WSL_BACKUP_DIR:-/home/administrator/ideai/backups}"
WIN_BACKUP_DIR="${WIN_BACKUP_DIR:-/mnt/d/Backups/Nexus}"
KEEP_COUNT="${KEEP_COUNT:-14}"

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

# ── Riepilogo ───────────────────────────────────────────────────────────────
echo
echo "==> Stato finale $WIN_BACKUP_DIR"
ls -lht "$WIN_BACKUP_DIR" | head -8
echo
echo "Totale: $(du -sh "$WIN_BACKUP_DIR" | cut -f1)"
echo "Spazio libero D:  $(df -h "$WIN_BACKUP_DIR" | awk 'NR==2 {print $4}')"
