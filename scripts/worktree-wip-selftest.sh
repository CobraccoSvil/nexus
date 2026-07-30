#!/usr/bin/env bash
# Test end-to-end di scripts/worktree-wip.ps1, sul caso che ha prodotto il danno:
# il worktree viene RIMOSSO dopo il salvataggio (archiviazione distruttiva).
#
# Perche' esiste come test e non come verifica una-tantum: un presidio anti-perdita
# che smette di funzionare in silenzio e' peggio di nessun presidio, perche' nel
# frattempo si smette di controllare a mano.
#
# Attraversa lo script REALE (powershell -File), non una sua imitazione, e verifica
# le quattro categorie di lavoro non committato una per una. Le prime due sono
# quelle che i due recuperi sbagliati perdono ciascuno per conto proprio:
#   (a) modifica in STAGING          -> la perde 'git diff' (usato il 30/07)
#   (c) file NUOVO non tracciato     -> la perde anche 'git diff HEAD'
#
# Test di mutazione (regola O), da rieseguire dopo ogni modifica allo script:
#   sed -i "s/'add', '-A'/'add', '-u'/" scripts/worktree-wip.ps1
#   bash scripts/worktree-wip-selftest.sh    # deve ROSSEGGIARE sul caso (c)
#   git checkout -- scripts/worktree-wip.ps1
# Misurato: con quella mutazione il confronto dei tree dentro -Restore resta VERDE
# (prova la fedelta' del ripristino, non la completezza della cattura); solo il
# caso (c) di questo test cade, col sintomo reale del 30/07: modulo assente.
#
# Uso: bash scripts/worktree-wip-selftest.sh
# Exit 0 = tutti i casi superati.
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && git rev-parse --path-format=absolute --git-common-dir)"
REPO="$(dirname "$REPO_ROOT")"
WT="$REPO-worktrees/wip-selftest"
BR=wip-selftest-tmp
SCRIPT_WIN="$(cd "$(dirname "$0")" && pwd -W 2>/dev/null || pwd)/worktree-wip.ps1"
SCRIPT_WIN="${SCRIPT_WIN//\//\\}"
run_ps() { powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$SCRIPT_WIN" "$@"; }

cleanup() {
  git -C "$REPO" worktree remove --force "$WT" >/dev/null 2>&1
  git -C "$REPO" branch -D "$BR" >/dev/null 2>&1
  git -C "$REPO" update-ref -d refs/wip/wip-selftest >/dev/null 2>&1
  rm -f /tmp/wip-selftest.idx
}
trap cleanup EXIT

echo "repo comune: $REPO"
echo "### 1. worktree usa-e-getta"
cleanup
git -C "$REPO" worktree add -b "$BR" "$WT" main >/dev/null 2>&1 || { echo "FALLITO worktree add"; exit 1; }

echo "### 2. quattro categorie di lavoro non committato"
echo "// (a) staged" >> "$WT/README.md"
git -C "$WT" add README.md
echo "// (b) non staged" >> "$WT/CLAUDE.md"
mkdir -p "$WT/crates/mcp-core/src/prova"
echo "pub fn nuovo() {}" > "$WT/crates/mcp-core/src/prova/modulo_nuovo.rs"
rm "$WT/lefthook.yml"
git -C "$WT" status --porcelain

echo
echo "### 3. cosa vede ciascun metodo di recupero"
printf '  %-44s %s\n' "git diff (usato il 30/07):" "$(git -C "$WT" diff --name-only 2>/dev/null | tr '\n' ' ')"
printf '  %-44s %s\n' "git diff HEAD:" "$(git -C "$WT" diff HEAD --name-only 2>/dev/null | tr '\n' ' ')"

echo
echo "### 4. salvataggio"
run_ps -Save >/dev/null 2>&1
WIP=$(git -C "$REPO" rev-parse refs/wip/wip-selftest 2>/dev/null)
if [ -z "$WIP" ]; then echo "FALLITO: nessun ref di salvataggio creato"; exit 1; fi
printf '  %-44s %s\n' "ref creato:" "${WIP:0:12}"
printf '  %-44s %s\n' "contenuto del salvataggio:" "$(git -C "$REPO" diff --name-only "$WIP^" "$WIP" | tr '\n' ' ')"
TREE_SALVATO=$(git -C "$REPO" rev-parse "$WIP^{tree}")

echo
echo "### 5. archiviazione distruttiva: il worktree viene rimosso"
git -C "$REPO" worktree remove --force "$WT" >/dev/null 2>&1 && echo "  worktree rimosso" || { echo "  FALLITA rimozione"; exit 1; }
if git -C "$REPO" rev-parse --verify --quiet refs/wip/wip-selftest >/dev/null; then
  echo "  il salvataggio sopravvive alla rimozione: SI"
else
  echo "  il salvataggio NON sopravvive: presidio inutile"; exit 1
fi

echo
echo "### 6. worktree ricreato, ripristino"
git -C "$REPO" worktree add "$WT" "$BR" >/dev/null 2>&1
run_ps -Restore wip-selftest -Into "${WT//\//\\}"
restore_exit=$?

echo
echo "### 7. verifica indipendente delle quattro categorie"
ok=0; ko=0
check() {
  if [ "$2" = "$3" ]; then echo "  OK    $1"; ok=$((ok+1));
  else echo "  ROTTO $1 (atteso '$3', trovato '$2')"; ko=$((ko+1)); fi
}
check "(a) modifica in staging" "$(grep -c '(a) staged' "$WT/README.md" 2>/dev/null || echo 0)" "1"
check "(b) modifica non in staging" "$(grep -c '(b) non staged' "$WT/CLAUDE.md" 2>/dev/null || echo 0)" "1"
check "(c) file nuovo non tracciato" "$([ -f "$WT/crates/mcp-core/src/prova/modulo_nuovo.rs" ] && echo presente || echo assente)" "presente"
check "(d) file cancellato" "$([ -f "$WT/lefthook.yml" ] && echo presente || echo assente)" "assente"
TREE_DOPO=$(cd "$WT" && GIT_INDEX_FILE=/tmp/wip-selftest.idx git read-tree HEAD >/dev/null 2>&1 &&
  GIT_INDEX_FILE=/tmp/wip-selftest.idx git add -A >/dev/null 2>&1 &&
  GIT_INDEX_FILE=/tmp/wip-selftest.idx git write-tree 2>/dev/null)
check "tree ripristinato identico al salvato" "$TREE_DOPO" "$TREE_SALVATO"
check "exit di -Restore" "$restore_exit" "0"

echo
echo "  risultato: $ok superati, $ko rotti"
[ "$ko" -eq 0 ] || exit 1
