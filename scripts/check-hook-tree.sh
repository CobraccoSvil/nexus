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
#
# CIO' CHE QUESTO GUARD NON PUO' VEDERE, e perche' non e' una lacuna da tappare.
#
#   L'08/08/2026 una sessione ha visto nell'output di 'turbo_quick' percorsi di
#   un ALTRO worktree, col guard verde. Non e' un buco del guard: e' una domanda
#   diversa. Qui si verifica DOVE girano i comandi; li' i comandi giravano
#   nell'albero giusto e semplicemente non sono stati eseguiti.
#
#   Turbo v2 tiene una cache condivisa fra tutti i worktree dello stesso
#   repository, e lo dichiara. Riprodotto da un worktree con node_modules
#   proprio, eseguendo 'turbo run typecheck --filter=@ai-orchestrator/types':
#
#     - Remote caching disabled, using shared worktree cache
#     @ai-orchestrator/types:typecheck: cache hit, replaying logs 410d93af7a9c3493
#     @ai-orchestrator/types:typecheck: > ... D:\IDEAI-worktrees\intelligent-mclaren-eb04d7\packages\types
#     Time: 3.177s >>> FULL TURBO
#
#   Nessun tsc e' partito in questo albero: l'entry era stata prodotta in un
#   altro worktree e ne e' stato ripristinato e ristampato il log (la cache sta
#   in <repo-principale>/.turbo/cache, e i manifest elencano <pkg>/.turbo/
#   turbo-<task>.log fra gli output — per questo il log ricompare, con i percorsi
#   assoluti di la').
#
#   Il replay non e' di per se' sbagliato: la chiave di turbo e' il CONTENUTO
#   degli input, quindi due alberi identici hanno lo stesso esito, ed e' lo
#   stesso meccanismo della remote cache fra macchine. Cio' che manca e' la
#   premessa: "typecheck OK" non dice che in questo albero non e' stato
#   eseguito niente. Estendere questo guard non aiuterebbe — il replay avviene
#   dentro turbo, dopo che questo script ha gia' concluso, e su un canale
#   (l'hash del contenuto) che qui non e' osservabile. Sta scritto qui perche' e'
#   qui che lo cerchera' il prossimo che vede percorsi altrui in un output.
set -euo pipefail

# Rete di sicurezza indipendente contro la trappola documentata di git
# (git-worktree(1), ENVIRONMENT VARIABLES): "If GIT_DIR is set but GIT_WORK_TREE
# is not, the current working directory is regarded as the top level of your
# working tree." Per l'hook di un worktree collegato, git imposta SEMPRE GIT_DIR
# prima di invocare l'hook; .lefthookrc (sourced prima che lefthook parta) fissa
# SEMPRE GIT_WORK_TREE per chiudere quella trappola per l'intera catena a valle
# (vedi commento li'). Se arriviamo qui con GIT_DIR presente e GIT_WORK_TREE
# assente, quel fix e' stato bypassato o rotto: qualunque comando 'git' a valle
# (compreso questo stesso script, un gate futuro, o un comando diagnostico)
# rischia di mescolare l'indice di un albero coi file fisici di un altro.
# Riprodotto e verificato (regola O): con questa combinazione uno script che fa
# 'git -C <altro-albero> status' mostra centinaia di file fantasma e puo'
# arrivare a scrivere nell'indice dell'altro repository.
if [ -n "${GIT_DIR:-}" ] && [ -z "${GIT_WORK_TREE:-}" ]; then
  echo "pre-commit: GIT_DIR e' nell'ambiente ma GIT_WORK_TREE no." >&2
  echo "" >&2
  echo "  GIT_DIR: $GIT_DIR" >&2
  echo "" >&2
  echo "Questa combinazione fa si' che qualunque comando 'git' con una CWD" >&2
  echo "diversa dal worktree tratti quella CWD come work-tree, mescolando" >&2
  echo "l'indice di un albero coi file fisici di un altro (git-worktree(1))." >&2
  echo ".lefthookrc dovrebbe gia' fissare GIT_WORK_TREE prima di questo punto:" >&2
  echo "verifica che non sia stato rimosso, bypassato, o che LEFTHOOK_BIN non" >&2
  echo "provenga da un'invocazione che salta il source di .lefthookrc." >&2
  echo "" >&2
  echo "Operazione git RIFIUTATA." >&2
  echo "Bypass volontario, solo emergenza: LEFTHOOK=0 git commit ..." >&2
  exit 1
fi

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
