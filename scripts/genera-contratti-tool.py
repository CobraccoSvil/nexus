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
import argparse
import io
import json
import re
from collections import defaultdict
from pathlib import Path

# La radice viene dal percorso di QUESTO file, non da un letterale: uno script
# che dichiara un albero e ne legge un altro misura un'imitazione del sistema
# (regola O — e' il difetto di `xtask quality-scan --root`, fix 2ae08818). In un
# worktree il letterale `D:/IDEAI` avrebbe generato i contratti dal catalogo del
# repo principale, cioe' da un file che il test di equivalenza non legge.
RADICE = Path(__file__).resolve().parents[1]
SCHEMA = RADICE / "crates/nexus-agent-tools/src/tool_schema.rs"

# Tool i cui valori dipendono dal PROGETTO o dal DB: l'enum e' rigenerato a
# runtime e un contratto statico lo fisserebbe (vedi il commento in
# input_contract.rs). Restano fuori DELIBERATAMENTE.
ENUM_DINAMICI = {"nexus_verify_change", "dispatch_subagent", "dispatch_subagents"}

# Campi in cui catalogo e handler NON dicono la stessa cosa: generare un
# contratto significherebbe scegliere quale dei due ha ragione, e quella e' una
# decisione, non una traduzione. Restano fuori finche' qualcuno non decide.
DECISIONE_UMANA = {
    ("nexus_search_semantic", "source_kinds"): (
        "lo schema promette 5 valori, l'handler ne accetta 8 via SourceKind::parse "
        "(mcp-core/src/rag/mod.rs) e scarta in silenzio i non riconosciuti"
    ),
}

# Il nome viene dal campo, ma il campo non sempre nomina la DOMANDA. Qui si
# dichiara, invece di inventare una regola che indovini.
NOMI_ESPLICITI = {
    # `Severity` esiste gia' in nexus-agent-graph::decisions::severity con un
    # altro significato (la gravita' di un'evidenza in una review). Questo e' il
    # livello di un toast: stesso nome, domanda diversa, e un nome distinto
    # impedisce che un domani qualcuno li unifichi credendoli lo stesso.
    "severity": "NotificationSeverity",
}

TIPI = {
    "string": "String",
    "boolean": "bool",
    "integer": "i64",
    "number": "f64",
}


def camel(s: str) -> str:
    return "".join(p.capitalize() for p in s.split("_"))


def enum_di(spec):
    """I valori ammessi del campo, sia dichiarati sul campo sia sui suoi items.

    Guardare il solo livello superiore era il difetto: `rel_types` e
    `source_kinds` portano l'enum DENTRO `items`, e generarli come `Vec<String>`
    toglieva al modello un vincolo che il catalogo gli da'.
    """
    if isinstance(spec.get("enum"), list):
        return tuple(spec["enum"]), False
    if spec.get("type") == "array":
        dentro = (spec.get("items") or {}).get("enum")
        if isinstance(dentro, list):
            return tuple(dentro), True
    return None, False


def tipo_rust(spec, nome_enum=None):
    """Il tipo Rust del campo, o None se la macro non lo sa esprimere."""
    valori, in_array = enum_di(spec)
    if valori:
        return f"Vec<{nome_enum}>" if in_array else nome_enum
    t = spec.get("type")
    if t in TIPI:
        return TIPI[t]
    if t == "array":
        items = spec.get("items") or {}
        if items.get("type") == "string":
            return "Vec<String>"
        if not items:
            # `items: {}` — una lista di valori su cui il catalogo non promette
            # nulla (i parametri posizionali di una query). `Vec<Value>` dice
            # esattamente questo, e lo schema che ne esce coincide.
            return "Vec<serde_json::Value>"
        return None  # array di oggetti: struttura annidata
    if t == "object":
        if spec.get("properties"):
            return None  # oggetto con forma dichiarata: struttura annidata
        return "serde_json::Map<String, serde_json::Value>"
    if t is None:
        return "serde_json::Value"  # esplicitamente senza vincolo
    return None


def escape(s: str) -> str:
    return s.replace("\\", "\\\\").replace('"', '\\"')


# 16 spazi di rientro + le virgolette, sotto i 120 caratteri che `quality-scan`
# considera riga lunga. Le descrizioni del catalogo arrivano a 325 caratteri:
# accorciarle cambierebbe cio' che il modello legge, quindi si spezzano.
LARGHEZZA = 96


def descrizione(testo: str, rientro: str, prefisso: int) -> str:
    """La descrizione come uno o piu' letterali adiacenti, che la macro concatena.

    Lo spazio di separazione resta attaccato alla parola che precede: e' il
    testo del catalogo a dover uscire identico, e il test di equivalenza lo
    verifica parola per parola.

    `prefisso` e' quanto occupa gia' la riga (rientro + `campo: Tipo, `): se la
    descrizione non ci sta accanto, va TUTTA a capo — lasciarne un pezzo li'
    terrebbe la riga lunga, che e' il difetto da cui si parte.
    """
    intero = escape(testo)
    if prefisso + len(intero) + 2 <= 120:
        return f'"{intero}"'
    pezzi, corrente = [], ""
    for parola in testo.split(" "):
        candidato = f"{corrente}{parola} "
        if corrente and len(escape(candidato)) > LARGHEZZA:
            pezzi.append(corrente)
            corrente = f"{parola} "
        else:
            corrente = candidato
    pezzi.append(corrente.rstrip(" ") if testo[-1] != " " else corrente)
    sep = f"\n{rientro}    "
    return sep + sep.join(f'"{escape(p)}"' for p in pezzi)


ap = argparse.ArgumentParser(description=__doc__)
ap.add_argument(
    "--out",
    type=Path,
    default=RADICE / "target/contratti-tool",
    help="dove scrivere contratti.rs ed elenco.rs (default: target/, ignorato da git)",
)
USCITA = ap.parse_args().out

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


# --- Censimento degli enum ------------------------------------------------
#
# Un tipo per INSIEME DI VALORI, non per campo: `encoding` di
# nexus_read_attachment e di nexus_read_archive_entry e' la stessa domanda, e
# `rel_type` (singolo) e `rel_types` (dentro un array) pure — due tipi identici
# sarebbero la duplicazione che questo lavoro toglie (regola L). Al contrario
# `verdict` porta due insiemi DIVERSI (review: pass/fail/needs_changes;
# advisory: proceed/…), quindi restano due tipi: stesso nome, domande diverse.
gruppi = defaultdict(set)  # valori -> nomi di campo che li usano
primo_tool = {}  # valori -> primo tool che li dichiara
for t in sorted(tools, key=lambda x: x.get("name", "")):
    if t.get("name") in ENUM_DINAMICI:
        continue
    for campo, spec in ((t.get("input_schema") or {}).get("properties") or {}).items():
        if not isinstance(spec, dict):
            continue
        valori, _ = enum_di(spec)
        if valori:
            gruppi[valori].add(campo)
            primo_tool.setdefault(valori, t["name"])

nomi_enum, per_nome = {}, defaultdict(list)
for valori, campi in gruppi.items():
    # Il campo piu' corto e' il singolare quando il gruppo ne ha due
    # (`rel_type` batte `rel_types`), ed e' l'unico quando ne ha uno.
    scelto = sorted(campi, key=lambda c: (len(c), c))[0]
    nomi_enum[valori] = NOMI_ESPLICITI.get(scelto, camel(scelto))
    per_nome[nomi_enum[valori]].append(valori)
for nome_candidato, collisi in per_nome.items():
    if len(collisi) == 1:
        continue
    for valori in collisi:
        tool = primo_tool[valori]
        # Il nome del tool distingue gia' i due sensi quando lo contiene
        # (`review_verdict`, `advisory_verdict`); altrimenti si concatena.
        nomi_enum[valori] = (
            camel(tool) if tool.endswith(nome_candidato.lower()) else camel(tool) + nome_candidato
        )

# Un valore che non e' un identificatore Rust non ha un nome di variante
# DEDUCIBILE (`1024x1024` inizia con una cifra). Il generatore non lo inventa:
# dichiara il tool fuori portata e il contratto si scrive a mano, dove un umano
# puo' dare alle varianti un nome che significhi qualcosa.
def variante(valore: str):
    c = camel(valore)
    return c if re.fullmatch(r"[A-Za-z][A-Za-z0-9]*", c) else None


enum_scritti, enum_impossibili = [], {}
for valori in sorted(gruppi, key=lambda v: nomi_enum[v]):
    varianti = [(variante(v), v) for v in valori]
    if any(nome is None for nome, _ in varianti):
        enum_impossibili[valori] = [v for n, v in varianti if n is None]
        continue
    righe = [f"crate::tool_enum! {{", f"    {nomi_enum[valori]} {{"]
    righe += [f'        {n} => "{v}";' for n, v in varianti]
    righe += ["    }", "}"]
    enum_scritti.append((nomi_enum[valori], "\n".join(righe)))

generati, esclusi, enum_usati = [], [], set()
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
    usati_dal_tool = set()
    for campo, spec in props.items():
        if not isinstance(spec, dict):
            problema = f"campo '{campo}' senza spec"
            break
        # I valori ammessi diventano un TIPO (`tool_enum!`), non una String:
        # generarli come stringa libera toglierebbe al modello un vincolo che il
        # catalogo gli da' oggi. Restano fuori solo quelli i cui valori non
        # danno un nome di variante deducibile.
        if (nome, campo) in DECISIONE_UMANA:
            problema = f"campo '{campo}': {DECISIONE_UMANA[(nome, campo)]}"
            break
        valori, _ = enum_di(spec)
        if valori in enum_impossibili:
            problema = (
                f"campo '{campo}': valori senza nome di variante deducibile "
                f"({', '.join(enum_impossibili[valori])})"
            )
            break
        rt = tipo_rust(spec, nomi_enum.get(valori))
        if rt is None:
            problema = f"campo '{campo}' di tipo {spec.get('type')} non esprimibile"
            break
        if valori:
            usati_dal_tool.add(nomi_enum[valori])
        prefisso = 12 + len(f"{campo}: {rt}, ")
        desc = descrizione(spec.get("description", ""), " " * 12, prefisso)
        (obb if campo in required else opz).append(f"{campo}: {rt}, {desc};")

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
    enum_usati |= usati_dal_tool

print(f"generabili : {len(generati)}")
print(f"enum       : {len(enum_usati)} dichiarati, usati da {len(generati)} contratti")
print(f"esclusi    : {len(esclusi)}")
for n, motivo in esclusi:
    print(f"   {n:38} {motivo}")

# Solo gli enum che un contratto generato usa davvero: dichiararne uno che
# nessuno nomina sarebbe codice morto, e il gate lo rifiuterebbe.
enum_da_scrivere = [c for n, c in enum_scritti if n in enum_usati]

USCITA.mkdir(parents=True, exist_ok=True)
io.open(USCITA / "contratti.rs", "w", encoding="utf-8", newline="\n").write(
    "\n\n".join(enum_da_scrivere + [c for _, _, c in generati]) + "\n"
)
io.open(USCITA / "elenco.rs", "w", encoding="utf-8", newline="\n").write(
    "\n".join(f'            ("{n}", <{s} as InputTool>::schema()),' for n, s, _ in generati)
    + "\n"
)
print(f"\nscritti contratti.rs ({len(generati)}) ed elenco.rs")
