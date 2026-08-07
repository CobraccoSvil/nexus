"""Genera le dichiarazioni `tool_input!` dal catalogo REALE dei tool.

I contratti si SCRIVONO una volta e devono coincidere con lo schema che il
modello legge: generarli dalla fonte che il modello legge davvero e' l'unico
modo di non introdurre divergenze proprio mentre si costruisce lo strumento che
le impedisce (regola O).

Classifica ogni tool in:
  - generabile: tutti i campi hanno un tipo che la macro sa esprimere
  - da_fare_a_mano: c'e' un campo che la macro non copre, col motivo

Non genera nulla per i secondi: un contratto approssimato sarebbe peggio di
nessun contratto, perche' il test di equivalenza lo dichiarerebbe conforme a uno
schema che non e' quello vero.
"""
import io
import json
import re
from pathlib import Path

RADICE = Path("D:/IDEAI")
SCHEMA = RADICE / "crates/nexus-agent-tools/src/tool_schema.rs"

# Tool i cui valori dipendono dal PROGETTO o dal DB: l'enum e' rigenerato a
# runtime e un contratto statico lo fisserebbe (vedi il commento in
# input_contract.rs). Restano fuori DELIBERATAMENTE.
ENUM_DINAMICI = {"nexus_verify_change", "dispatch_subagent", "dispatch_subagents"}

TIPI = {
    "string": "String",
    "boolean": "bool",
    "integer": "i64",
    "number": "f64",
}


def tipo_rust(spec):
    """Il tipo Rust del campo, o None se la macro non lo sa esprimere."""
    t = spec.get("type")
    if t in TIPI:
        return TIPI[t]
    if t == "array":
        items = spec.get("items") or {}
        if items.get("type") == "string":
            return "Vec<String>"
        return None  # array di oggetti: struttura annidata
    if t == "object":
        return None  # oggetto annidato
    if t is None:
        return "serde_json::Value"  # esplicitamente senza vincolo
    return None


def escape(s: str) -> str:
    return s.replace("\\", "\\\\").replace('"', '\\"')


sorgente = io.open(SCHEMA, encoding="utf-8").read()
blocchi = re.findall(r'r#"(\s*\[.*?\])\s*"#', sorgente, re.S)
tools = []
for b in blocchi:
    try:
        tools.extend(json.loads(b))
    except Exception:
        pass


def nome_struct(nome_tool: str) -> str:
    return "".join(p.capitalize() for p in nome_tool.split("_")) + "Input"


generati, esclusi = [], []
for t in sorted(tools, key=lambda x: x.get("name", "")):
    nome = t.get("name", "")
    if not nome:
        continue
    if nome in ENUM_DINAMICI:
        esclusi.append((nome, "enum rigenerato a runtime (progetto/DB)"))
        continue
    sch = t.get("input_schema") or {}
    props = sch.get("properties") or {}
    required = set(sch.get("required") or [])

    obb, opz, problema = [], [], None
    for campo, spec in props.items():
        if not isinstance(spec, dict):
            problema = f"campo '{campo}' senza spec"
            break
        # Un campo con enum STATICO resta una String: il vincolo lo esprime gia'
        # il catalogo, e replicarlo come enum Rust vorrebbe dire scriverlo due
        # volte — cioe' il difetto che questo lavoro esiste per togliere.
        # Un campo con `enum` porta un VINCOLO che la macro non sa esprimere:
        # generarlo come stringa libera lo perderebbe, cioe' toglierebbe al
        # modello una restrizione che oggi ha. Meglio nessun contratto che uno
        # che promette meno del catalogo.
        if isinstance(spec.get("enum"), list):
            problema = f"campo '{campo}' ha un enum che la macro non esprime"
            break
        rt = tipo_rust(spec)
        if rt is None:
            problema = f"campo '{campo}' di tipo {spec.get('type')} non esprimibile"
            break
        riga = f'{campo}: {rt}, "{escape(spec.get("description", ""))}";'
        (obb if campo in required else opz).append(riga)

    if problema:
        esclusi.append((nome, problema))
        continue
    if not props:
        esclusi.append((nome, "nessun parametro"))
        continue

    corpo = [f"crate::tool_input! {{", f"    {nome_struct(nome)} for \"{nome}\" {{"]
    corpo.append("        obbligatori {")
    for r in obb:
        corpo.append(f"            {r}")
    corpo.append("        }")
    corpo.append("        opzionali {")
    for r in opz:
        corpo.append(f"            {r}")
    corpo.append("        }")
    corpo.append("    }")
    corpo.append("}")
    generati.append((nome, nome_struct(nome), "\n".join(corpo)))

print(f"generabili : {len(generati)}")
print(f"esclusi    : {len(esclusi)}")
for n, motivo in esclusi:
    print(f"   {n:38} {motivo}")

USCITA = Path(
    "C:/Users/CBRAC/AppData/Local/Temp/claude/D--IDEAI/2727b270-3073-4d91-ba79-955d4ad7e3ca/scratchpad"
)
io.open(USCITA / "contratti.rs", "w", encoding="utf-8", newline="\n").write(
    "\n\n".join(c for _, _, c in generati) + "\n"
)
io.open(USCITA / "elenco.rs", "w", encoding="utf-8", newline="\n").write(
    "\n".join(f'            ("{n}", <{s} as InputTool>::schema()),' for n, s, _ in generati)
    + "\n"
)
print(f"\nscritti contratti.rs ({len(generati)}) ed elenco.rs")
