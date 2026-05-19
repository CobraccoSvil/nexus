#!/usr/bin/env bash
set -euo pipefail
cd /home/administrator/ideai
git add crates/mcp-core/src/agent_tools/safety.rs \
        crates/mcp-quality/src/lib.rs \
        crates/mcp-ast/src/lib.rs \
        crates/mcp-core/src/project_workspace/scan_ports.rs \
        crates/mcp-learning/src/lib.rs \
        crates/mcp-core/src/nexus_tools/secret_scan.rs \
        crates/mcp-core/src/nexus_tools/sast_scan.rs \
        scripts/unwrap-perfile-v2.py \
        scripts/classify-unwrap.py \
        scripts/commit-phase3-step1.sh

git commit -m "$(cat <<'EOF'
chore(rust): annota cluster Regex literal unwrap come safety (§F)

Baseline iniziale Fase 3 indicava 446 unwrap + 53 expect "fuori test".
Riconteggio robusto (script `unwrap-perfile-v2.py` con detector cfg(test)
context-aware) ridimensiona la cifra:

  PROD: 128 unwrap + 23 expect = 151 occorrenze totali
  TEST: 316 unwrap + 24 expect = 340 (legittime)

Classificazione PROD (script `classify-unwrap.py`):

  REGEX (literal hardcoded, ammesso §F): 93
  PARSE static literal:                   1
  OTHER (da analizzare):                  57

I 93 Regex literal sono concentrati in 6 file. Aggiunto un commento
`// safety:` in testa a ciascuno che cita CLAUDE.md §F (clausola
"Conversioni da static literals dove l'impossibilita' e' dimostrata")
e annota il refactor opportuno (`std::sync::LazyLock<Regex>`):

  - crates/mcp-quality/src/lib.rs            (29 occorrenze)
  - crates/mcp-ast/src/lib.rs                (16 occorrenze)
  - crates/mcp-core/src/project_workspace/scan_ports.rs (14)
  - crates/mcp-learning/src/lib.rs           (8)
  - crates/mcp-core/src/nexus_tools/secret_scan.rs (7)
  - crates/mcp-core/src/nexus_tools/sast_scan.rs (6)

Più: crates/mcp-core/src/agent_tools/safety.rs — l'unica `.expect()`
di compile-time RegexSet ora ha annotazione safety esplicita.

`cargo check --workspace` continua a passare. Le 57 occorrenze OTHER
saranno affrontate in un commit successivo (env bootstrap, Mutex
poisoned, SHA256 conversion, Option::unwrap reali).

Tool: `scripts/unwrap-perfile-v2.py` (count PROD vs TEST robusto) e
`scripts/classify-unwrap.py` (categorizzazione semantica).
EOF
)"

git log -1 --stat
