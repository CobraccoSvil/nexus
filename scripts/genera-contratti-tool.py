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

# Campi i cui valori ammessi li fissa il RUNTIME (il profilo del progetto, il
# registry DB): il contratto li dichiara con `tool_enum_dinamico!`, che porta il
# SEED nello schema — dove `apply_verify_scope_enum` lo sostituisce — e lascia
# il parsing a una stringa. Un enum Rust qui rifiuterebbe alla deserializzazione
# proprio i valori che il catalogo rigenerato ha promesso al modello.
#
# Chiave `(tool, campo)` -> nome del tipo. Il campo puo' essere annidato
# (`tasks[].kind` di dispatch_subagents): li' conta il solo nome.
ENUM_DINAMICI = {
    ("nexus_verify_change", "scope"): "ScopeVerifica",
    ("dispatch_subagent", "kind"): "KindSubagente",
    ("dispatch_subagents", "kind"): "KindSubagente",
}

# Campi in cui catalogo e handler NON dicono la stessa cosa: generare un
# contratto significherebbe scegliere quale dei due ha ragione, e quella e' una
# decisione, non una traduzione. Restano fuori finche' qualcuno non decide.
#
# Vuoto: l'unico caso noto (`nexus_search_semantic.source_kinds`, schema con 5
# valori contro gli 8 di `SourceKind::parse`) e' stato deciso il 08/08/2026
# allineando lo schema al vocabolario — le tre sorgenti in piu' sono collection
# vive, che un commento dichiarava gia' «esposte tramite il canale unico
# nexus_search_semantic», e il vocabolario ora e' un punto unico condiviso.
DECISIONE_UMANA = {}

# Vocabolari che hanno GIA' un punto unico: il contratto ci DELEGA invece di
# dichiarare un gemello. Due enum con gli stessi valori sarebbero la
# duplicazione nella sua forma peggiore — nessun compilatore obbliga a tenerli
# allineati, e la divergenza si scopre in esercizio.
#
# Il tipo deve essere raggiungibile da `nexus-agent-tools` e implementare
# `TipoJson`: e' la ragione per cui `Severity` e' migrato in `nexus-types`
# (`decisions/severity.rs` resta un re-export, come per i tier).
TIPI_CONDIVISI = {
    ("alta", "media", "bassa"): "nexus_types::severity::Severity",
    (
        "attachment",
        "kb",
        "chat_history",
        "tool_result",
        "code",
        "meta_doc",
        "conversation",
        "prompt_correction",
    ): "nexus_types::source_kind::SourceKind",
}

# Il nome viene dal campo, ma il campo non sempre nomina la DOMANDA. Chiavato
# sui VALORI e non sul nome del campo: lo stesso campo puo' portare vocabolari
# diversi in tool diversi, ed e' il vocabolario che il tipo rappresenta.
NOMI_ESPLICITI = {
    # Distinto da `Severity` di decisions::severity, che e' la gravita' di
    # un'evidenza: questo e' il livello di un toast. Un nome distinto impedisce
    # che un domani qualcuno li unifichi credendoli lo stesso.
    ("info", "success", "warning", "error"): "NotificationSeverity",
}

# Un campo del catalogo puo' chiamarsi come una parola riservata Rust (`type`,
# in `nexus_todo_write`). L'identificatore grezzo `r#type` e' un nome valido, e
# serde ne toglie il prefisso da solo; a toglierlo dallo SCHEMA ci pensa
# `schema_oggetto`, dove `stringify!` lo conserverebbe.
RISERVATE_RUST = {
    "as", "box", "break", "const", "continue", "crate", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
    "move", "mut", "pub", "ref", "return", "self", "static", "struct", "super",
    "trait", "true", "type", "unsafe", "use", "where", "while", "async", "await",
    "dyn", "abstract", "become", "do", "final", "macro", "override", "priv",
    "typeof", "unsized", "virtual", "yield",
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


def tipo_rust(spec, nome_enum=None, annidato=None):
    """Il tipo Rust del campo, o None se la macro non lo sa esprimere.

    `annidato` e' la funzione che dichiara un oggetto con forma nota e ne
    ritorna il nome del tipo: passata dal chiamante perche' sa a quale TOOL e
    campo appartiene, e il nome ne discende.
    """
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
        if items.get("type") == "object" and items.get("properties") and annidato:
            return f"Vec<{annidato(items)}>"
        return None
    if t == "object":
        if spec.get("properties"):
            return annidato(spec) if annidato else None
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
    # +3: le due virgolette e il punto e virgola che chiude la riga. Contarne
    # due lasciava passare le righe di esattamente 121 caratteri.
    if prefisso + len(intero) + 3 <= 120:
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
def campi_ovunque(schema):
    """Ogni (campo, spec) dello schema, ANCHE dentro gli oggetti annidati.

    Il censimento fermo al primo livello lasciava senza tipo gli enum di
    `direction`, `severity` e `status`, che vivono dentro gli oggetti di
    advisory_verdict, debate_position e nexus_todo_write: tre tool esclusi per
    un enum che il censimento non aveva guardato.
    """
    for campo, spec in (schema.get("properties") or {}).items():
        if not isinstance(spec, dict):
            continue
        yield campo, spec
        dentro = spec if spec.get("type") == "object" else (spec.get("items") or {})
        if isinstance(dentro, dict) and dentro.get("properties"):
            yield from campi_ovunque(dentro)


gruppi = defaultdict(set)  # valori -> nomi di campo che li usano
primo_tool = {}  # valori -> primo tool che li dichiara
for t in sorted(tools, key=lambda x: x.get("name", "")):
    for campo, spec in campi_ovunque(t.get("input_schema") or {}):
        if (t.get("name"), campo) in ENUM_DINAMICI:
            continue  # il seed non e' un vocabolario: non entra fra gli enum
        valori, _ = enum_di(spec)
        if valori:
            gruppi[valori].add(campo)
            primo_tool.setdefault(valori, t["name"])

nomi_enum, per_nome = {}, defaultdict(list)
for valori, campi in gruppi.items():
    # Il campo piu' corto e' il singolare quando il gruppo ne ha due
    # (`rel_type` batte `rel_types`), ed e' l'unico quando ne ha uno.
    scelto = sorted(campi, key=lambda c: (len(c), c))[0]
    if valori in TIPI_CONDIVISI:
        # Nessun enum da dichiarare: il tipo esiste gia' e il contratto lo usa.
        nomi_enum[valori] = TIPI_CONDIVISI[valori]
        continue
    nomi_enum[valori] = NOMI_ESPLICITI.get(valori, camel(scelto))
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


# I tipi a valori dinamici: un `tool_enum_dinamico!` per NOME (KindSubagente e'
# lo stesso tipo per i due dispatch), col seed preso dal catalogo.
dinamici_scritti, seed_visto = [], {}
for t in tools:
    for campo, spec in campi_ovunque(t.get("input_schema") or {}):
        tipo = ENUM_DINAMICI.get((t.get("name"), campo))
        if not tipo:
            continue
        valori = enum_di(spec)[0] or ()
        if tipo in seed_visto:
            assert seed_visto[tipo] == valori, f"{tipo}: due seed diversi"
            continue
        seed_visto[tipo] = valori
        righe = ["crate::tool_enum_dinamico! {", f"    {tipo} {{", "        seed {"]
        righe += [f'            "{v}";' for v in valori]
        righe += ["        }", "    }", "}"]
        dinamici_scritti.append((tipo, "\n".join(righe)))

enum_scritti, enum_impossibili = [], {}
for valori in sorted(gruppi, key=lambda v: nomi_enum[v]):
    if valori in TIPI_CONDIVISI:
        continue  # il tipo esiste gia' altrove: qui non si dichiara nulla
    varianti = [(variante(v), v) for v in valori]
    if any(nome is None for nome, _ in varianti):
        enum_impossibili[valori] = [v for n, v in varianti if n is None]
        continue
    righe = [f"crate::tool_enum! {{", f"    {nomi_enum[valori]} {{"]
    righe += [f'        {n} => "{v}";' for n, v in varianti]
    righe += ["    }", "}"]
    enum_scritti.append((nomi_enum[valori], "\n".join(righe)))

# --- Oggetti annidati -----------------------------------------------------
#
# Come per gli enum, un tipo per FORMA e non per campo: `risks` di
# advisory_verdict e di debate_position sono la stessa struttura, e due tipi
# identici sarebbero duplicazione (regola L). Il nome viene dal primo uso.
oggetti_scritti, nomi_oggetto, oggetti_falliti = [], {}, {}


def singolare(campo: str) -> str:
    return campo[:-1] if campo.endswith("s") and not campo.endswith("ss") else campo


def dichiara_oggetto(tool: str, campo: str, forma: dict) -> str:
    """Dichiara un `tool_object!` per questa forma e ne ritorna il nome del tipo."""
    chiave = json.dumps(forma, sort_keys=True)
    if chiave in nomi_oggetto:
        return nomi_oggetto[chiave]
    # Anche i fallimenti si ricordano, e si RILANCIANO: `risks` ha la stessa
    # forma in advisory_verdict e debate_position, e registrando il nome prima
    # di saper dichiarare il tipo il secondo tool lo usava come se esistesse —
    # un contratto che nomina un tipo mai generato, cioe' un errore di
    # compilazione al posto di un'esclusione motivata.
    if chiave in oggetti_falliti:
        raise ValueError(oggetti_falliti[chiave])
    nome = camel(tool) + camel(singolare(campo))
    props = forma.get("properties") or {}
    req = set(forma.get("required") or [])
    obb, opz = [], []
    def fallisci(motivo: str):
        oggetti_falliti[chiave] = motivo
        raise ValueError(motivo)

    for c, sp in props.items():
        dinamico = ENUM_DINAMICI.get((tool, c))
        valori = enum_di(sp)[0]
        # Ricorsivo perche' il catalogo annida a due livelli: gli
        # `acceptance_criteria` di `nexus_todo_write` stanno dentro i `todos`,
        # che stanno dentro l'input. Senza questa riga quel tool restava fuori
        # per un campo che la macro sa esprimere benissimo.
        rt = dinamico or tipo_rust(
            sp,
            nomi_enum.get(valori),
            lambda forma, _c=c: dichiara_oggetto(tool, _c, forma),
        )
        if rt is None:
            fallisci(f"campo annidato '{c}' di tipo {sp.get('type')} non esprimibile")
        if valori and not dinamico:
            enum_usati.add(nomi_enum[valori])
        nome_campo = f"r#{c}" if c in RISERVATE_RUST else c
        prefisso = 12 + len(f"{nome_campo}: {rt}, ")
        (obb if c in req else opz).append(
            f"{nome_campo}: {rt}, {descrizione(sp.get('description', ''), ' ' * 12, prefisso)};"
        )
    righe = ["crate::tool_object! {", f"    {nome} {{", "        obbligatori {"]
    righe += [f"            {r}" for r in obb]
    righe += ["        }", "        opzionali {"]
    righe += [f"            {r}" for r in opz]
    righe += ["        }", "    }", "}"]
    nomi_oggetto[chiave] = nome
    oggetti_scritti.append((nome, "\n".join(righe)))
    return nome


generati, esclusi, enum_usati = [], [], set()
for t in sorted(tools, key=lambda x: x.get("name", "")):
    nome = t.get("name", "")
    if not nome:
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
        dinamico = ENUM_DINAMICI.get((nome, campo))
        valori, _ = enum_di(spec)
        if valori in enum_impossibili:
            problema = (
                f"campo '{campo}': valori senza nome di variante deducibile "
                f"({', '.join(enum_impossibili[valori])})"
            )
            break
        try:
            rt = dinamico or tipo_rust(
                spec,
                nomi_enum.get(valori),
                lambda forma, _c=campo: dichiara_oggetto(nome, _c, forma),
            )
        except ValueError as e:
            problema = str(e)
            break
        if rt is None:
            problema = f"campo '{campo}' di tipo {spec.get('type')} non esprimibile"
            break
        if valori and not dinamico:
            usati_dal_tool.add(nomi_enum[valori])
        obbligatorio = campo in required
        nome_campo = f"r#{campo}" if campo in RISERVATE_RUST else campo
        prefisso = 12 + len(f"{nome_campo}: {rt}, ")
        desc = descrizione(spec.get("description", ""), " " * 12, prefisso)
        (obb if obbligatorio else opz).append(f"{nome_campo}: {rt}, {desc};")

    if problema:
        esclusi.append((nome, problema))
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
print(f"oggetti    : {len(oggetti_scritti)} annidati dichiarati")
print(f"esclusi    : {len(esclusi)}")
for n, motivo in esclusi:
    print(f"   {n:38} {motivo}")

# Solo cio' che un contratto generato usa DAVVERO: un tool escluto per un campo
# puo' aver gia' fatto dichiarare i suoi oggetti annidati, e un tipo che nessuno
# nomina e' codice morto che il gate rifiuta. La chiusura e' transitiva perche'
# un oggetto puo' contenerne un altro (`todos` -> `acceptance_criteria`).
testo_contratti = "\n".join(c for _, _, c in generati)
usati, cambiato = set(), True
while cambiato:
    cambiato = False
    for n, c in oggetti_scritti:
        if n in usati:
            continue
        visibile = re.search(rf"\b{n}\b", testo_contratti) or any(
            re.search(rf"\b{n}\b", corpo) for altro, corpo in oggetti_scritti if altro in usati
        )
        if visibile:
            usati.add(n)
            cambiato = True
oggetti_scritti = [(n, c) for n, c in oggetti_scritti if n in usati]
for _, corpo in oggetti_scritti:
    enum_usati |= {n for n, _ in enum_scritti if re.search(rf"\b{n}\b", corpo)}
enum_da_scrivere = [c for n, c in enum_scritti if n in enum_usati]

USCITA.mkdir(parents=True, exist_ok=True)
io.open(USCITA / "contratti.rs", "w", encoding="utf-8", newline="\n").write(
    "\n\n".join(
        [c for _, c in dinamici_scritti]
        + enum_da_scrivere
        + [c for _, c in oggetti_scritti]
        + [c for _, _, c in generati]
    )
    + "\n"
)
io.open(USCITA / "elenco.rs", "w", encoding="utf-8", newline="\n").write(
    "\n".join(f'            ("{n}", <{s} as InputTool>::schema()),' for n, s, _ in generati)
    + "\n"
)
print(f"\nscritti contratti.rs ({len(generati)}) ed elenco.rs")
