"""Estrazione Fase B: nexus_tools (meno 9 accoppiati) + sandbox + audit/quotas
in crate workspace `nexus-tool-kit`. Churn minimo via re-export.
"""
import os
import re
import subprocess

ROOT = "/home/administrator/ideai"
os.chdir(ROOT)

CRATE = "crates/nexus-tool-kit"
SRC = f"{CRATE}/src"
NT = "crates/mcp-core/src/nexus_tools"

# Tool che RESTANO in mcp-core (usano NexusBridge / nexus_tool_catalog)
KEEP = {
    "consensus_vote", "memory_ns",
    "ruvector_insert", "ruvector_search", "ruvector_stats",
    "meta_catalog_count", "meta_categories_list",
    "meta_health_summary", "meta_self_test",
}


def run(cmd):
    r = subprocess.run(cmd, shell=True, capture_output=True, text=True)
    if r.returncode != 0:
        raise SystemExit(f"FALLITO: {cmd}\n{r.stderr[:800]}")
    return r.stdout


os.makedirs(SRC, exist_ok=True)

# 1. Sposta i file tool estraibili
moved = []
for f in sorted(os.listdir(NT)):
    if not f.endswith(".rs") or f == "mod.rs":
        continue
    name = f[:-3]
    if name in KEEP:
        continue
    run(f"git mv {NT}/{f} {SRC}/{f}")
    moved.append(name)
print(f"spostati {len(moved)} tool")

# 2. Sposta sandbox / audit / quotas
run(f"git mv crates/mcp-core/src/sandbox.rs {SRC}/sandbox.rs")
run(f"git mv crates/mcp-core/src/security/audit.rs {SRC}/audit.rs")
run(f"git mv crates/mcp-core/src/security/quotas.rs {SRC}/quotas.rs")

# 3. Spartizione del mod.rs: il corpo comune va in lib.rs; le dichiarazioni
#    `pub mod X;` vengono smistate (estratte vs keep).
mod_text = open(f"{NT}/mod.rs").read()
decl_re = re.compile(r"^(pub )?mod ([a-z_0-9]+);\s*$", re.MULTILINE)
keep_decls = []
lib_decls = []
for m in decl_re.finditer(mod_text):
    name = m.group(2)
    (keep_decls if name in KEEP else lib_decls).append(name)
body = decl_re.sub("", mod_text)

lib = []
lib.append('//! nexus-tool-kit — handler dei tool Nexus estratti dal monolite mcp-core')
lib.append('//! (split 7.4 fase B). Contiene il trait `NexusToolHandler` + runtime')
lib.append('//! (context, errori, safety), gli helper comuni e ~%d tool puri.' % len(moved))
lib.append('//! I 9 tool accoppiati a NexusBridge/nexus_tool_catalog restano in')
lib.append('//! mcp-core (`src/nexus_tools/`), che re-esporta questo crate per')
lib.append('//! mantenere validi i path `crate::nexus_tools::*` storici.')
lib.append("")
lib.append("pub mod audit;")
lib.append("pub mod quotas;")
lib.append("pub mod sandbox;")
for name in sorted(lib_decls):
    lib.append(f"pub mod {name};")
lib.append("")
lib.append(body)
open(f"{SRC}/lib.rs", "w").write("\n".join(lib))

# 4. mod.rs residuo in mcp-core
res = []
res.append("//! Tool Nexus residenti in mcp-core: i 9 accoppiati a NexusBridge /")
res.append("//! nexus_tool_catalog. Tutto il resto vive nel crate nexus-tool-kit")
res.append("//! (split 7.4 fase B): il re-export sottostante mantiene validi i path")
res.append("//! `crate::nexus_tools::*` per i ~70 moduli che li usano.")
res.append("pub use nexus_tool_kit::*;")
res.append("")
for name in sorted(keep_decls):
    res.append(f"pub mod {name};")
res.append("")
open(f"{NT}/mod.rs", "w").write("\n".join(res))

# 5. Fix path nei file del crate nuovo
for f in os.listdir(SRC):
    if not f.endswith(".rs"):
        continue
    p = os.path.join(SRC, f)
    t = open(p, errors="replace").read()
    orig = t
    t = t.replace("crate::security::quotas", "crate::quotas")
    t = t.replace("crate::security::record_audit", "crate::audit::record_audit")
    t = t.replace("crate::security::AuditEntry", "crate::audit::AuditEntry")
    t = t.replace("crate::project_db::", "nexus_project_db::")
    t = t.replace("crate::nexus_tools::", "crate::")
    if t != orig:
        open(p, "w").write(t)

# 6. mcp-core: main.rs (mod sandbox -> re-export) e security/mod.rs
main = open("crates/mcp-core/src/main.rs").read()
main = main.replace(
    "mod sandbox;",
    "pub use nexus_tool_kit::sandbox;",
)
open("crates/mcp-core/src/main.rs", "w").write(main)

sec = open("crates/mcp-core/src/security/mod.rs").read()
sec = sec.replace("pub mod audit;", "pub use nexus_tool_kit::audit;")
sec = sec.replace("pub mod quotas;", "pub use nexus_tool_kit::quotas;")
open("crates/mcp-core/src/security/mod.rs", "w").write(sec)

# 7. Cargo.toml del crate: censimento use esterni
ext = set()
use_re = re.compile(r"^\s*(?:pub )?use ([a-z_0-9]+)(::|;)", re.MULTILINE)
path_re = re.compile(r"(?<![:\w])([a-z_][a-z_0-9]*)::")
for f in os.listdir(SRC):
    if f.endswith(".rs"):
        t = open(os.path.join(SRC, f), errors="replace").read()
        for m in use_re.finditer(t):
            ext.add(m.group(1))
        for m in path_re.finditer(t):
            ext.add(m.group(1))
ext -= {"crate", "super", "self", "std", "core", "alloc"}
print("riferimenti esterni censiti:", sorted(ext))

print("FATTO — scrivere Cargo.toml a mano dai riferimenti sopra")
