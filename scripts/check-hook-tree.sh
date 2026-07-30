#!/usr/bin/env bash
# scripts/check-hook-tree.sh — I gate girano nell'albero che sta committando?
#
# Perche' esiste (fail-closed, come .lefthookrc):
#   Gli hook git sono CONDIVISI fra il repo principale e tutti i suoi worktree:
#   core.hooksPath punta a <main>/.git/hooks per ogni worktree (qui e' scritto
#   per-worktree in .git/worktrees/<nome>/config.worktree dal tooling che crea
#   il worktree, ma il file eseguito e' comunque uno solo). L'albero in cui i
#   comandi girano NON e' quindi garantito da nulla di dichiarato: e' un effetto
#   collaterale di come git invoca l'hook e di come lefthook risolve la propria
#   project root.
#
#   Se quell'effetto collaterale cambia — altra versione di lefthook, un
#   LEFTHOOK_CONFIG che punta altrove, un'invocazione con CWD diversa — i gate
#   continuano a girare, ma sull'ALTRO albero: leggono altri sorgenti, altre
#   baseline, altro Cargo.lock. Nessuno fallisce. Il pre-commit resta verde e
#   non dice piu' niente sul commit in corso: assenza di misura sull'oggetto
#   giusto travestita da misura superata, cioe' il falso verde che .lefthookrc
#   esiste per impedire, nella sua seconda meta' (regola O di CLAUDE.md: lo
#   strumento di misura deve raggiungere il suo oggetto come la produzione).
#
# Cosa confronta (e perche' NON e' una tautologia):
#   'git rev-parse --show-toplevel' NON serve: segue la CWD (misurato: con
#   GIT_DIR del worktree e CWD nel repo principale restituisce il repo
#   principale). Confrontarlo con pwd sarebbe lo strumento che misura se stesso.
#   L'albero che sta committando si legge invece da --absolute-git-dir, che
#   viene da GIT_DIR — imposto da git all'hook, indipendente dalla CWD.
#
#   Contro quell'unica fonte indipendente si verificano i due modi in cui un
#   gate puo' finire sull'albero sbagliato:
#     1. la CWD dei comandi   -> dove girano;
#     2. la posizione di questo script (relativa, come per gli altri gate, che
#        si ancorano con 'cd "$(dirname "$0")/.."') -> di quale albero sono i
#        sorgenti, le baseline e i manifest che i gate leggono.
#
# Sta in lefthook.yml con 'priority: 1' (supportato da v1.13.6, verificato) e
# senza glob: gira prima di ogni gate e per qualunque tipo di file staged, cosi'
# copre anche i gate che verranno aggiunti con altri glob.
set -euo pipefail

# Normalizzatore unico: gli stessi due path scritti in due modi (separatori,
# 8.3, case del drive) sarebbero una divergenza inventata. Una sola forma.
_normalizza() {
  (cd "$1" 2>/dev/null && (pwd -W 2>/dev/null || pwd)) || echo "$1"
}

_rifiuta() {
  echo "pre-commit: i gate NON stanno girando nell'albero che sta committando." >&2
  echo "" >&2
  echo "  albero che committa (da GIT_DIR): ${albero_atteso:-<non determinabile>}" >&2
  echo "  cwd dei comandi:                  ${cwd:-<ignota>}" >&2
  echo "  albero di questo script:          ${albero_script:-<ignoto>}" >&2
  echo "" >&2
  echo "$1" >&2
  echo "" >&2
  echo "Operazione git RIFIUTATA: un gate che misura l'albero sbagliato non e'" >&2
  echo "un gate superato -- sarebbe verde qualunque cosa contenga il commit." >&2
  echo "Bypass volontario, solo emergenza: LEFTHOOK=0 git commit ..." >&2
  exit 1
}

cwd="$(_normalizza ".")"
albero_script="$(_normalizza "$(dirname "$0")/..")"

# L'albero che sta committando, dalla sola fonte che non segue la CWD.
admin_dir="$(git rev-parse --absolute-git-dir 2>/dev/null || true)"
common_dir="$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null || true)"

albero_atteso=""
if [ -n "$admin_dir" ] && [ -n "$common_dir" ]; then
  if [ "$admin_dir" = "$common_dir" ]; then
    # Repo principale: l'albero e' la cartella che contiene la git dir.
    albero_atteso="$(_normalizza "$(dirname "$common_dir")")"
  elif [ -f "$admin_dir/gitdir" ]; then
    # Worktree: l'admin dir dichiara il proprio albero nel file 'gitdir'
    # (contiene '<albero>/.git'). E' git a scriverlo, non un'euristica sui path.
    _gitdir_ref="$(tr -d '\r\n' < "$admin_dir/gitdir")"
    if [ -n "$_gitdir_ref" ]; then
      albero_atteso="$(_normalizza "$(dirname "$_gitdir_ref")")"
    fi
  fi
fi

# Non determinabile != determinato uguale: senza la fonte indipendente non c'e'
# confronto possibile, e un confronto impossibile non e' un confronto superato.
if [ -z "$albero_atteso" ]; then
  _rifiuta "Rimedio: la git dir non e' interrogabile ('git rev-parse --absolute-git-dir'
e '--path-format=absolute --git-common-dir'). Verifica di essere in un repo git
e che la versione di git supporti --path-format (2.31+)."
fi

if [ "$cwd" != "$albero_atteso" ]; then
  _rifiuta "I comandi girano in un albero diverso da quello del commit: i gate
misurerebbero sorgenti che non stai committando. Causa tipica: lefthook risolve
la propria project root altrove (versione diversa, LEFTHOOK_CONFIG impostato)."
fi

if [ "$albero_script" != "$albero_atteso" ]; then
  _rifiuta "Gli script dei gate provengono da un albero diverso da quello del
commit: si ancorano al proprio path con 'cd \"\$(dirname \"\$0\")/..\"', quindi
leggerebbero sorgenti, baseline e manifest dell'albero sbagliato. Causa tipica:
lefthook invoca i comandi con path assoluto verso un altro albero."
fi
