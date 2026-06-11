#!/usr/bin/env python3
"""Censimento configurazioni `settings`: DB live + migrazioni vs lettori nel codice vs UI.

Punto unico (regola L) per l'audit "ogni setting esposta in admin e' davvero
letta dal codice". Quattro collettori:

  A1. DB live          — SELECT key, category FROM settings (docker exec psql)
  A2. Migrazioni       — parser di INSERT/DELETE su db/migrations/*.sql
  B.  Lettori codice   — regex sulle API punto-unico (nexus-auth / settings_db)
                         + SQL diretto + pattern dinamici whitelistati
  C.  UI admin         — categorie navigabili dalla sidebar (admin-sidebar.tsx)

Classificazione per chiave del DB live:
  VIVA        in DB + letta dal codice (literal, prefisso, wildcard o categoria)
  MORTA       in DB + nessun lettore trovato
  FANTASMA    letta literal nel codice ma assente dal DB live
  INVISIBILE  in DB + letta, ma categoria non raggiungibile dalla UI admin
  RUNTIME     in DB, non nelle migrazioni, scritta da codice noto (whitelist)
  TEST-ONLY   letta solo da file di test

Uso (wrapper: scripts/audit-settings.sh):
  python3 scripts/audit_settings.py --report          # tabella riassuntiva
  python3 scripts/audit_settings.py --json out.json   # dump completo
  python3 scripts/audit_settings.py --no-db           # senza DB (solo A2 vs B)
  python3 scripts/audit_settings.py --gate            # exit!=0 su regressioni vs baseline

La riconciliazione dei call site e' parte del contratto: ogni chiamata ai
lettori che NON produce una chiave literal viene stampata (file:riga) per
revisione manuale — il residuo deve restare spiegato dalla whitelist dinamica.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# ---------------------------------------------------------------------------
# Pattern dinamici noti (whitelist con motivazione). Una chiave del DB che
# matcha uno di questi pattern conta come LETTA anche senza literal nel codice.
# Tenere allineata ai call site citati nel commento.
# ---------------------------------------------------------------------------
DYNAMIC_READ_PATTERNS: list[tuple[str, str]] = [
    # environment.rs:738,1203 + model_catalog_sync.rs (format!("{}_api_key", provider))
    # + brain/providers/api_key_loader.py
    (r".*_api_key$", "chiavi API provider lette per pattern <provider>_api_key"),
    # playwright_install.rs:395 — chiave per-progetto creata/letta a runtime
    (r"^project:[0-9a-f-]+:playwright_enabled$", "flag playwright per-progetto"),
    # brain/agents/nodes/helpers.py:86,147,2722 — prefissi LIKE
    (r"^agent\.iteration_budget\..*", "LIKE agent.iteration_budget.% (helpers.py)"),
    (r"^agent\.complexity\..*", "LIKE agent.complexity.% (helpers.py)"),
    (r"^agent\.tier_floor\..*", "LIKE agent.tier_floor.% (helpers.py)"),
    (r"^agent\.context\..*", "LIKE agent.context.% (helpers.py)"),
]

# Categorie lette PER INTERO dal codice (SELECT ... WHERE category = '<cat>').
# Call site: brain/agents/meta_steps.py:63, clarify_or_expand_node.py:117,
# brain/agents/orchestrator_config.py, environment.rs:738 (providers).
CATEGORY_BULK_READERS: dict[str, str] = {
    "orchestrator": "meta_steps.py / clarify_or_expand_node.py / orchestrator_config.py",
    "providers": "environment.rs (api keys per categoria)",
}

# Chiavi scritte a runtime da codice (non da migrazione): non sono morte.
RUNTIME_WRITTEN_KEYS: list[tuple[str, str]] = [
    (r"^model_catalog_last_sync$", "models.rs:341 (timestamp sync)"),
    (r"^project:.*", "playwright_install.rs (chiavi per-progetto)"),
    (r"^jwt_secret$", "nexus-auth get_or_create_jwt_secret"),
]

# Eccezioni deliberate: nessun lettore nel codice ma MANTENUTE per contratto
# (verifica adversariale audit 2026-06-11, vedi ADR 0031). Non contano come
# morte nel gate.
KEEP_DESPITE_NO_READER: dict[str, str] = {
    "agent.visual_compare.similarity_threshold":
        "citata nel contratto testuale dei system prompt attivi (mig 0215)",
    "gitlab_personal_access_token":
        "contratto secret_bindings del plugin gitlab-stdio (risoluzione "
        "dinamica in plugin-service resolve_secret_value)",
}

EXCLUDE_DIRS = {
    "node_modules", "target", ".next", "__pycache__", ".git", ".turbo",
    "generated", "dist", "build", ".dup-report", "recovery", ".venv",
}

# ---------------------------------------------------------------------------
# A1 — DB live
# ---------------------------------------------------------------------------
def collect_db_live() -> dict[str, str] | None:
    cmd = [
        "docker", "exec", "ideai-postgres-nexus-1",
        "psql", "-U", "nexus", "-d", "nexus", "-t", "-A", "-F", "|",
        "-c", "SELECT key, category FROM settings ORDER BY key",
    ]
    try:
        out = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.TimeoutExpired):
        return None
    if out.returncode != 0:
        return None
    rows: dict[str, str] = {}
    for line in out.stdout.splitlines():
        if "|" in line:
            key, _, cat = line.partition("|")
            rows[key.strip()] = cat.strip()
    return rows or None


# ---------------------------------------------------------------------------
# A2 — Migrazioni: INSERT INTO settings / DELETE FROM settings
# ---------------------------------------------------------------------------
def _split_sql_tuples(body: str) -> list[list[str]]:
    """Spezza il body di un VALUES in tuple, rispettando apici e parentesi."""
    tuples: list[list[str]] = []
    depth = 0
    in_str = False
    cur = ""
    fields: list[str] = []
    i = 0
    while i < len(body):
        ch = body[i]
        if in_str:
            if ch == "'":
                if i + 1 < len(body) and body[i + 1] == "'":  # apice escapato ''
                    cur += "'"
                    i += 2
                    continue
                in_str = False
            else:
                cur += ch
        else:
            if ch == "'":
                in_str = True
            elif ch == "(":
                depth += 1
                if depth == 1:
                    fields = []
                    cur = ""
                    i += 1
                    continue
                cur += ch
            elif ch == ")":
                depth -= 1
                if depth == 0:
                    fields.append(cur.strip())
                    tuples.append(fields)
                    cur = ""
                    i += 1
                    continue
                cur += ch
            elif ch == "," and depth == 1:
                fields.append(cur.strip())
                cur = ""
            elif depth >= 1:
                cur += ch
        i += 1
    return tuples


def collect_migrations() -> tuple[dict[str, tuple[str, str]], set[str]]:
    """Ritorna (chiavi inserite -> (categoria, file)), chiavi cancellate."""
    inserted: dict[str, tuple[str, str]] = {}
    deleted: set[str] = set()
    mig_dir = ROOT / "db" / "migrations"
    ins_re = re.compile(
        r"INSERT\s+INTO\s+settings\s*\(([^)]*)\)\s*VALUES\s*(.*?);",
        re.IGNORECASE | re.DOTALL,
    )
    del_eq_re = re.compile(
        r"DELETE\s+FROM\s+settings\s+WHERE\s+key\s*=\s*'([^']+)'", re.IGNORECASE)
    del_in_re = re.compile(
        r"DELETE\s+FROM\s+settings\s+WHERE\s+key\s+IN\s*\(([^)]*)\)", re.IGNORECASE)

    for sql_file in sorted(mig_dir.glob("*.sql")):
        text = sql_file.read_text(encoding="utf-8", errors="replace")
        for m in ins_re.finditer(text):
            cols = [c.strip().lower() for c in m.group(1).split(",")]
            if "key" not in cols:
                continue
            key_idx = cols.index("key")
            cat_idx = cols.index("category") if "category" in cols else None
            for tup in _split_sql_tuples(m.group(2)):
                if key_idx < len(tup):
                    key = tup[key_idx]
                    if not key or "SELECT" in key.upper() or "||" in key:
                        continue  # INSERT..SELECT o chiave costruita: fuori scope
                    cat = tup[cat_idx] if cat_idx is not None and cat_idx < len(tup) else ""
                    inserted[key] = (cat, sql_file.name)
                    deleted.discard(key)
        for m in del_eq_re.finditer(text):
            deleted.add(m.group(1))
            inserted.pop(m.group(1), None)
        for m in del_in_re.finditer(text):
            for raw in m.group(1).split(","):
                k = raw.strip().strip("'")
                if k:
                    deleted.add(k)
                    inserted.pop(k, None)
    return inserted, deleted


# ---------------------------------------------------------------------------
# B — Lettori nel codice
# ---------------------------------------------------------------------------
RUST_READER_RE = re.compile(
    r"\b(get_setting_checked|get_setting_nonempty|get_setting|get_bool_setting"
    r"|get_int_setting|resolve_port)\s*\(\s*[^,()]*,\s*\"([^\"]+)\"",
    re.DOTALL,
)
PY_READER_RE = re.compile(
    r"\b(get_setting_checked|get_bool_setting_checked|get_int_setting_checked"
    r"|get_setting|get_bool_setting|get_int_setting|resolve_port)\s*\(\s*"
    r"(?:key\s*=\s*)?[\"']([^\"']+)[\"']"
)
SQL_KEY_EQ_RE = re.compile(
    r"FROM\s+settings\s+WHERE\s+key\s*=\s*'([^']+)'", re.IGNORECASE)
# Call site dei lettori che NON hanno chiave literal (per riconciliazione).
RUST_CALLSITE_RE = re.compile(
    r"\b(get_setting_checked|get_setting_nonempty|get_setting|get_bool_setting"
    r"|get_int_setting|resolve_port)\s*\(")
PY_CALLSITE_RE = RUST_CALLSITE_RE  # stessi nomi


def _walk(root: Path, exts: tuple[str, ...]):
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if d not in EXCLUDE_DIRS]
        for fn in filenames:
            if fn.endswith(exts):
                yield Path(dirpath) / fn


def _is_test_path(p: Path) -> bool:
    s = str(p)
    return ("/tests/" in s or "/test_" in s or s.endswith("_test.rs")
            or "/__tests__/" in s or ".test." in s or ".spec." in s)


QUOTED_RE = re.compile(r"\"([^\"\\\n]{2,120})\"|'([^'\\\n]{2,120})'")
# Chiavi d'oggetto JS/TS non quotate (es. DB_KEY_MAP del gateway:
# `rate_limit_per_tenant_window_ms: "RATE_..."`): senza questo pattern
# risultavano false-morte.
TS_BAREKEY_RE = re.compile(r"^\s*([a-z][a-z0-9_]{3,60}):", re.MULTILINE)


def collect_code_readers() -> tuple[dict[str, list[str]], list[str], set[str]]:
    """Ritorna (chiave -> [siti file:riga]), call site non riconciliati,
    e il set di TUTTE le stringhe quotate nei sorgenti.

    Il set di stringhe quotate e' il rilevatore primario di "riferita dal
    codice": molte chiavi sono lette via dict di default batch
    (parse_typed_settings, key = ANY(...)) o wrapper locali, non solo dai 7
    lettori canonici. Una chiave mai citata in NESSUN sorgente e' morta con
    alta confidenza; il contrario (citata ma morta) e' il falso sicuro.
    """
    readers: dict[str, list[str]] = defaultdict(list)
    unresolved: list[str] = []
    quoted: set[str] = set()
    # packages/ incluso: il gateway legge settings via DB_KEY_MAP con chiavi
    # di oggetto NON quotate (rate_limit_*), individuate solo scansionando
    # anche packages/shared e packages/llm-gateway (falso-morto evitato).
    scan_roots = [ROOT / "crates", ROOT / "brain", ROOT / "apps",
                  ROOT / "packages", ROOT / "scripts", ROOT / "evals",
                  ROOT / "deploy", ROOT / "config"]
    for root in scan_roots:
        if not root.exists():
            continue
        for path in _walk(root, (".rs", ".py", ".ts", ".tsx", ".sh", ".yaml", ".yml")):
            # Lo script stesso e il punto unico non sono "lettori di business".
            if path.name in ("audit_settings.py", "settings_db.py") or \
               str(path).endswith("crates/nexus-auth/src/lib.rs"):
                continue
            try:
                text = path.read_text(encoding="utf-8", errors="replace")
            except OSError:
                continue
            rel = str(path.relative_to(ROOT))
            for m in QUOTED_RE.finditer(text):
                quoted.add(m.group(1) or m.group(2))
            if path.suffix in (".ts", ".tsx"):
                for m in TS_BAREKEY_RE.finditer(text):
                    quoted.add(m.group(1))
            matched_spans: set[int] = set()
            # ATTENZIONE alle firme: in Rust la chiave e' il 2o argomento
            # (dopo &db), in Python il 1o. Applicare la regex sbagliata
            # cattura i valori di DEFAULT come chiavi (falsi fantasma).
            if path.suffix == ".rs":
                regs = [RUST_READER_RE]
            elif path.suffix == ".py":
                regs = [PY_READER_RE]
            else:
                regs = []
            for reg in regs:
                for m in reg.finditer(text):
                    line = text.count("\n", 0, m.start()) + 1
                    readers[m.group(2)].append(f"{rel}:{line}")
                    matched_spans.add(m.start())
            for m in SQL_KEY_EQ_RE.finditer(text):
                line = text.count("\n", 0, m.start()) + 1
                readers[m.group(1)].append(f"{rel}:{line}")
            # Riconciliazione: call site lettori senza literal riconosciuto.
            if path.suffix in (".rs", ".py"):
                for m in RUST_CALLSITE_RE.finditer(text):
                    if m.start() not in matched_spans:
                        line = text.count("\n", 0, m.start()) + 1
                        snippet = text[m.start():m.start() + 80].split("\n")[0]
                        unresolved.append(f"{rel}:{line}  {snippet}")
    return dict(readers), unresolved, quoted


# ---------------------------------------------------------------------------
# C — UI admin: categorie navigabili
# ---------------------------------------------------------------------------
def collect_ui_categories() -> set[str] | None:
    """Ritorna le categorie navigabili dalla UI admin, o None = TUTTE.

    Dalla bonifica 2026-06-11 la sidebar deriva le voci dai dati
    (lib/settings-categories.ts + GET /api/admin/settings-categories): ogni
    categoria del DB e' navigabile per costruzione, quindi la classe
    INVISIBILE non puo' piu' esistere finche' il punto unico dinamico e' in
    uso. Se il file sparisse (regressione a liste hardcoded), si torna al
    censimento statico di sidebar/CATEGORY_ORDER.
    """
    dynamic = ROOT / "apps/web-ide/lib/settings-categories.ts"
    if dynamic.exists():
        text = dynamic.read_text(encoding="utf-8", errors="replace")
        if "useSettingsCategories" in text and "settings-categories" in text:
            return None  # sidebar dinamica: tutte le categorie raggiungibili
    cats: set[str] = set()
    sidebar = ROOT / "apps/web-ide/components/admin-sidebar.tsx"
    if sidebar.exists():
        text = sidebar.read_text(encoding="utf-8", errors="replace")
        for m in re.finditer(r"/admin/settings/([a-z0-9_-]+)", text):
            cats.add(m.group(1))
    panel = ROOT / "apps/web-ide/components/settings/settings-panel.tsx"
    if panel.exists():
        text = panel.read_text(encoding="utf-8", errors="replace")
        m = re.search(r"CATEGORY_ORDER\s*=\s*\[([^\]]*)\]", text)
        if m:
            for c in re.findall(r"[\"']([a-z0-9_-]+)[\"']", m.group(1)):
                cats.add(c)
    return cats


# ---------------------------------------------------------------------------
# Classificazione
# ---------------------------------------------------------------------------
def classify(db_live, migrations, deleted_keys, readers, unresolved, ui_cats, quoted):
    dynamic = [(re.compile(p), why) for p, why in DYNAMIC_READ_PATTERNS]
    runtime = [(re.compile(p), why) for p, why in RUNTIME_WRITTEN_KEYS]

    def read_via(key: str, category: str):
        if key in KEEP_DESPITE_NO_READER:
            return "keep-exception"
        if key in readers:
            sites = readers[key]
            if all(_is_test_path(ROOT / s.split(":")[0]) for s in sites):
                return "test-only"
            return "literal"
        if key in quoted:
            return "quoted"
        for reg, _why in dynamic:
            if reg.match(key):
                return "dynamic"
        if category in CATEGORY_BULK_READERS:
            return "category"
        return None

    result = {
        "viva": {}, "morta": {}, "fantasma": {}, "invisibile": {},
        "runtime_only": {}, "test_only": {},
    }
    keyset_db = set(db_live or {})
    for key, cat in (db_live or {}).items():
        via = read_via(key, cat)
        is_runtime = key not in migrations and any(r.match(key) for r, _ in runtime)
        if via is None:
            if key not in migrations:
                # Non in migrazioni e non whitelistata: probabile scrittura
                # runtime non censita -> da revisionare, NON cancellare alla cieca.
                result["runtime_only"][key] = cat
            else:
                result["morta"][key] = cat
        elif via == "test-only":
            result["test_only"][key] = cat
        else:
            if ui_cats is not None and cat not in ui_cats:
                result["invisibile"][key] = cat
            else:
                result["viva"][key] = cat
            if is_runtime:
                result["runtime_only"].setdefault(key, cat)
    # Filtro forma-chiave: esclude default/valori catturati per errore
    # ("foo", "5", URL) — una chiave vera ha namespace con . o _ .
    keylike = re.compile(r"^[a-z][a-z0-9_.:-]*$")
    for key in sorted(set(readers) - keyset_db):
        if db_live is None:
            break
        if not keylike.match(key) or ("." not in key and "_" not in key) \
           or "://" in key or ":" in key.split(".")[0]:
            continue
        sites = readers[key]
        if all(_is_test_path(ROOT / s.split(":")[0]) for s in sites):
            continue
        result["fantasma"][key] = sites[:3]
    return result


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--report", action="store_true")
    ap.add_argument("--json", metavar="FILE")
    ap.add_argument("--no-db", action="store_true")
    ap.add_argument("--gate", action="store_true")
    ap.add_argument("--baseline", default=str(ROOT / "scripts/audit-settings-baseline.json"))
    args = ap.parse_args()

    db_live = None if args.no_db else collect_db_live()
    if db_live is None and not args.no_db:
        print("AVVISO: DB live non raggiungibile, procedo in modalita --no-db", file=sys.stderr)
    migrations, deleted = collect_migrations()
    readers, unresolved, quoted = collect_code_readers()
    ui_cats = collect_ui_categories()
    res = classify(db_live, migrations, deleted, readers, unresolved, ui_cats, quoted)

    counts = {k: len(v) for k, v in res.items()}
    summary = {
        "db_live_keys": len(db_live or {}),
        "migration_keys": len(migrations),
        "reader_keys": len(readers),
        "unresolved_call_sites": len(unresolved),
        "ui_categories": "dinamiche (tutte navigabili)" if ui_cats is None else sorted(ui_cats),
        **counts,
    }

    if args.json:
        payload = {"summary": summary, "classi": res, "unresolved": unresolved}
        Path(args.json).write_text(json.dumps(payload, indent=2, ensure_ascii=False))
        print(f"JSON scritto in {args.json}")

    if args.report or not (args.json or args.gate):
        print("=== audit settings: riepilogo ===")
        for k, v in summary.items():
            if k != "ui_categories":
                print(f"  {k}: {v}")
        if ui_cats is None:
            print("  ui_categories: dinamiche dal DB (tutte navigabili)")
        else:
            print(f"  ui_categories ({len(ui_cats)}): {', '.join(sorted(ui_cats))}")
        for cls in ("morta", "fantasma", "invisibile", "runtime_only", "test_only"):
            if res[cls]:
                print(f"\n--- {cls.upper()} ({len(res[cls])}) ---")
                for key in sorted(res[cls]):
                    print(f"  {key}  [{res[cls][key]}]")
        if unresolved:
            print(f"\n--- CALL SITE NON RICONCILIATI ({len(unresolved)}) ---")
            for u in unresolved[:60]:
                print(f"  {u}")
            if len(unresolved) > 60:
                print(f"  ... e altri {len(unresolved) - 60}")

    if args.gate:
        base_path = Path(args.baseline)
        cur = {"morta": counts["morta"], "fantasma": counts["fantasma"],
               "invisibile": counts["invisibile"]}
        if not base_path.exists():
            base_path.write_text(json.dumps(cur, indent=2) + "\n")
            print(f"Baseline creata: {base_path}")
            return 0
        base = json.loads(base_path.read_text())
        regress = {k: (cur[k], base.get(k, 0)) for k in cur if cur[k] > base.get(k, 0)}
        if regress:
            print(f"GATE FALLITO (regressioni vs baseline): {regress}", file=sys.stderr)
            return 1
        print(f"GATE OK: {cur} <= baseline {base}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
