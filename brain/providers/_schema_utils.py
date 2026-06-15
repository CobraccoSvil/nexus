"""Utilita' condivise per compressione schemi JSON dei tool definitions.

Lo scopo e' ridurre il peso del JSON inviato ai provider AI (BP6 del piano di
riduzione token). Rimuove campi non supportati o ridondanti e tronca testi
lunghi mantenendo l'informazione essenziale per l'LLM.

Vedi piano: docs/plans/audit-token-reduction.md sezione 4.1.
"""
from __future__ import annotations

from typing import Any

# Chiavi rimosse perche' non supportate da alcuni provider o ridondanti.
# - additionalProperties / $schema / default: non supportate da Google genai
# - examples: utili per test ma raddoppiano il peso senza guadagno per l'LLM
# - title: ridondante quando description e' presente
# - $defs / definitions: i nostri tool non usano referenze JSON Schema
_SKIP_KEYS = {
    "additionalProperties",
    "$schema",
    "default",
    "examples",
    "title",
    "$defs",
    "definitions",
}

# Limiti di compressione (centralizzati per coerenza fra provider).
DEFAULT_DESCR_MAX = 200
DEFAULT_ENUM_MAX = 10
DEFAULT_TOOL_DESCR_MAX = 400


def _truncate_text(value: str, limit: int) -> str:
    """Tronca testo a 'limit' caratteri preservando la prima frase quando possibile."""
    if len(value) <= limit:
        return value
    # Prova a tagliare al punto piu' vicino al limite (preserva frase).
    cut = value.rfind(".", 0, limit)
    if cut > limit // 2:
        return value[: cut + 1]
    return value[: limit - 3].rstrip() + "..."


def compress_schema(
    schema: dict,
    *,
    descr_max: int = DEFAULT_DESCR_MAX,
    enum_max: int = DEFAULT_ENUM_MAX,
) -> dict:
    """Comprimi uno schema JSON ricorsivamente.

    Operazioni:
    - rimuove chiavi in _SKIP_KEYS
    - tronca 'description' a descr_max caratteri
    - tronca 'enum' a enum_max valori (i restanti sono indicati con '...')
    - applica ricorsivamente a 'properties', 'items', 'oneOf', 'anyOf', 'allOf'
    """
    if not isinstance(schema, dict):
        return schema
    cleaned: dict = {}
    for k, v in schema.items():
        if k in _SKIP_KEYS:
            continue
        if k == "description" and isinstance(v, str):
            cleaned[k] = _truncate_text(v, descr_max)
        elif k == "enum" and isinstance(v, list) and len(v) > enum_max:
            # Mantiene i primi enum_max valori + sentinel per indicare troncamento.
            cleaned[k] = list(v[:enum_max]) + ["..."]
        elif k == "properties" and isinstance(v, dict):
            cleaned[k] = {
                pk: compress_schema(pv, descr_max=descr_max, enum_max=enum_max)
                if isinstance(pv, dict)
                else pv
                for pk, pv in v.items()
            }
        elif k in ("items", "additionalItems") and isinstance(v, dict):
            cleaned[k] = compress_schema(v, descr_max=descr_max, enum_max=enum_max)
        elif k in ("oneOf", "anyOf", "allOf") and isinstance(v, list):
            cleaned[k] = [
                compress_schema(item, descr_max=descr_max, enum_max=enum_max)
                if isinstance(item, dict)
                else item
                for item in v
            ]
        elif isinstance(v, dict):
            cleaned[k] = compress_schema(v, descr_max=descr_max, enum_max=enum_max)
        else:
            cleaned[k] = v
    return cleaned


def compress_tool_definition(
    tool: dict,
    *,
    schema_key: str = "input_schema",
    tool_descr_max: int = DEFAULT_TOOL_DESCR_MAX,
    descr_max: int = DEFAULT_DESCR_MAX,
    enum_max: int = DEFAULT_ENUM_MAX,
) -> dict:
    """Comprimi una definition di tool nel formato Anthropic.

    Esempio input:
        {"name": "...", "description": "...", "input_schema": {...}}

    schema_key permette di adattarsi al formato OpenAI ('parameters') o
    Google FunctionDeclaration ('parameters') quando serve.
    """
    out: dict[str, Any] = {}
    for k, v in tool.items():
        if k == "description" and isinstance(v, str):
            out[k] = _truncate_text(v, tool_descr_max)
        elif k == schema_key and isinstance(v, dict):
            out[k] = compress_schema(v, descr_max=descr_max, enum_max=enum_max)
        else:
            out[k] = v
    return out


def compress_tool_list(
    tools: list[dict],
    *,
    schema_key: str = "input_schema",
    tool_descr_max: int = DEFAULT_TOOL_DESCR_MAX,
    descr_max: int = DEFAULT_DESCR_MAX,
    enum_max: int = DEFAULT_ENUM_MAX,
) -> list[dict]:
    """Applica compress_tool_definition a tutta la lista.

    Helper di convenienza per i provider che ricevono una list[dict].
    """
    return [
        compress_tool_definition(
            t,
            schema_key=schema_key,
            tool_descr_max=tool_descr_max,
            descr_max=descr_max,
            enum_max=enum_max,
        )
        for t in tools
    ]


def is_first_agent_turn(messages: list[dict]) -> bool:
    """Determina se siamo al primo turno agente.

    Implementazione estesa rispetto alla definizione letterale "nessun tool_result
    nella history". Restituisce True anche quando l'agente ha eseguito SOLO
    tool di discovery (`nexus_mcp_tool_search`, `nexus_mcp_tool_call`) senza
    aver mai invocato un tool "concreto" che agisce sul progetto. Motivazione:
    se il modello ha CERCATO i tool ma poi vuole chiudere narrando ("Ora eseguo
    i test...") senza chiamare il tool concreto, va riforzato `tool_choice` per
    obbligare l'azione (incident chat 6 Beauty-Book run 2026-06-04 12:08).

    Detection:
    - role='tool' o block type='tool_result' con tool_name in DISCOVERY_TOOLS
      -> ancora "primo turno effettivo".
    - role='tool' o block type='tool_result' con tool_name diverso da discovery
      -> ESCE dal primo turno (l'agente ha agito).

    NB: alcuni adapter non popolano tool_name nel tool_result. Per quei casi
    correlo via tool_use_id col precedente tool_use (sempre presente nei
    messaggi assistant). Fallback conservativo: se non riesco a mappare il
    tool_name, considero come tool concreto (esce dal primo turno) per
    evitare loop di forzatura.
    """
    discovery = _DISCOVERY_TOOLS
    # Indice tool_use_id -> tool_name dai messaggi assistant.
    tool_use_names: dict[str, str] = {}
    for m in messages:
        if m.get("role") == "assistant":
            content = m.get("content")
            if isinstance(content, list):
                for blk in content:
                    if not isinstance(blk, dict):
                        continue
                    if blk.get("type") == "tool_use":
                        tid = blk.get("id") or ""
                        nm = blk.get("name") or ""
                        if tid and nm:
                            tool_use_names[tid] = nm

    for m in messages:
        role = m.get("role", "")
        if role == "tool":
            # Formato OpenAI-compat: name campo top-level
            tname = (m.get("name") or "").strip()
            if not tname:
                tcid = m.get("tool_call_id") or ""
                tname = tool_use_names.get(tcid, "")
            # Se non risolto, conservativo: trattalo come tool concreto.
            if not tname or tname not in discovery:
                return False
        content = m.get("content")
        if isinstance(content, list):
            for block in content:
                if not isinstance(block, dict) or block.get("type") != "tool_result":
                    continue
                tuid = block.get("tool_use_id") or ""
                tname = tool_use_names.get(tuid, "")
                if not tname or tname not in discovery:
                    return False
    return True


# Tool che NON contano come "azione concreta": l'agente li usa solo per
# scoprire altri tool. `nexus_mcp_tool_call` invece ESEGUE il tool risolto
# (azione vera), quindi NON e' discovery.
_DISCOVERY_TOOLS = frozenset(
    {"nexus_mcp_tool_search"}
)


def resolve_tool_choice_openai(
    model: str,
    messages: list[dict],
    *,
    weak_models: tuple[str, ...] = (),
    force_override: bool | None = None,
) -> str:
    """Determina il tool_choice per provider con API OpenAI-compatible.

    Ritorna:
    - "required" al primo turno per modelli non-weak (forza una tool call)
    - "auto" ai turni successivi o per modelli weak

    I modelli in `weak_models` usano sempre "auto" perche' "required"
    causa loop di safety-refusal (osservato su mistral-small, ministral).

    `force_override` (ADR 0018 leva 2): True forza "required" anche oltre il
    primo turno; False disattiva la forzatura (retry-senza-forcing); None =
    comportamento storico. I modelli weak restano sempre "auto".
    """
    if weak_models and any(tag in model.lower() for tag in weak_models):
        return "auto"
    if force_override is False:
        return "auto"
    if force_override is True:
        return "required"
    if is_first_agent_turn(messages):
        return "required"
    return "auto"


def measure_tools_bytes(tools: list[dict]) -> int:
    """Calcola il peso JSON serializzato di una lista di tool definitions.

    Utile per metriche before/after nei test e nel logging.
    """
    import json

    return len(json.dumps(tools, ensure_ascii=False, separators=(",", ":")))


def _coerce_value(raw: str):
    """Best-effort: stringa -> int/float/bool/None se possibile."""
    lower = raw.lower()
    if lower in ("true", "false"):
        return lower == "true"
    if lower == "null":
        return None
    try:
        return int(raw)
    except ValueError:
        pass
    try:
        return float(raw)
    except ValueError:
        pass
    return raw


def _parse_function_call_args(arg_string: str) -> dict:
    """Parsa i kwargs di una function-call testuale (es. ``tool_name(a="x", b=True)``).

    Riceve SOLO la porzione interna alle parentesi. Usa ``ast`` per robustezza
    (virgole dentro le stringhe, bool/None/numeri) senza MAI eseguire codice
    (niente ``eval``). Gli argomenti non interpretabili come literal diventano
    testo grezzo best-effort. Ritorna ``{}`` se vuoto o non parsabile.
    """
    import ast

    arg_string = (arg_string or "").strip()
    if not arg_string:
        return {}
    params: dict = {}
    try:
        node = ast.parse(f"_f({arg_string})", mode="eval")
        call = node.body
        if isinstance(call, ast.Call):
            for kw in call.keywords:
                if kw.arg is None:
                    continue
                try:
                    params[kw.arg] = ast.literal_eval(kw.value)
                except Exception:
                    try:
                        params[kw.arg] = ast.unparse(kw.value)
                    except Exception:
                        params[kw.arg] = ""
            # argomenti posizionali eventuali ignorati: i tool Nexus usano kwargs
            return params
    except Exception:
        pass
    # Fallback regex best-effort (key=value separati da virgola)
    import re as _re

    for m in _re.finditer(
        r"([A-Za-z_]\w*)\s*=\s*('[^']*'|\"[^\"]*\"|[^,]+)", arg_string
    ):
        key = m.group(1)
        val = m.group(2).strip()
        if (val.startswith('"') and val.endswith('"')) or (
            val.startswith("'") and val.endswith("'")
        ):
            params[key] = val[1:-1]
        else:
            params[key] = _coerce_value(val)
    return params


def parse_inline_tool_invocations(
    text: str,
    known_tool_names: set[str] | None = None,
) -> tuple[list[dict], str]:
    """Parser di recupero per tool_call emessi come XML inline nel content.

    Riconosce cinque formati:
      1. <invoke name="X"><parameter name="Y">V</parameter></invoke>
      2. <tool_name><param>value</param></tool_name>  (formato semplificato)
      3. <functions><function><name>X</name><params><k>v</k></params></function></functions>
         (formato DeepSeek/ChatGPT-style)
      4. <execute_tool><tool_name>X</tool_name><args><k>v</k></args></execute_tool>
         (wrapper "execute_tool": tool_name esplicito + figli di <args>)
      5. <execute_bash> X(k=v, ...) </execute_bash>  oppure function-call NUDA
         X(k=v, ...)  (la nuda solo se X in `known_tool_names`)

    I formati 2 e 5b (function-call nuda) richiedono `known_tool_names` per
    distinguere una vera invocazione da tag/parentesi arbitrarie nel testo: e'
    il gate anti-falso-positivo. I formati 1/3/4 e il wrapper 5a (<execute_bash>)
    sono auto-qualificati dal tag esplicito e non richiedono la whitelist.

    Ritorna ([], text_originale) se nessuna invocazione rilevata.
    """
    import re
    import uuid as _uuid

    if not text:
        return [], text

    # --- Normalizzazione tag DSML Unicode (DeepSeek) ---
    # DeepSeek emette tag con prefisso U+FF5C: <｜｜DSML｜｜invoke ...>
    # Li normalizziamo a tag XML standard per permettere il parsing.
    import re as _re_dsml
    _DSML_PREFIX = "｜｜DSML｜｜"
    if _DSML_PREFIX in text:
        text = text.replace(f"<{_DSML_PREFIX}invoke", "<invoke")
        text = text.replace(f"</{_DSML_PREFIX}invoke>", "</invoke>")
        text = text.replace(f"<{_DSML_PREFIX}parameter", "<parameter")
        text = text.replace(f"</{_DSML_PREFIX}parameter>", "</parameter>")
        text = _re_dsml.sub(
            rf"</?{_re_dsml.escape(_DSML_PREFIX)}tool_calls\s*/?>", "", text
        )
        text = _re_dsml.sub(r"\n{3,}", "\n\n", text).strip()

    blocks: list[dict] = []
    cleaned = text

    # --- Formato 1: <invoke name="X">...</invoke> ---
    if "<invoke" in text:
        invoke_re = re.compile(
            r'<invoke\s+name="(?P<name>[^"]+)"\s*>(?P<body>.*?)</invoke>',
            re.DOTALL,
        )
        param_re = re.compile(
            r'<parameter\s+name="(?P<pname>[^"]+)"(?:\s+string="(?P<is_string>true|false)")?\s*>(?P<value>.*?)</parameter>',
            re.DOTALL,
        )
        for m in invoke_re.finditer(text):
            tool_name = m.group("name").strip()
            body = m.group("body") or ""
            params: dict = {}
            for pm in param_re.finditer(body):
                pname = pm.group("pname").strip()
                raw_value = (pm.group("value") or "").strip()
                is_string_attr = pm.group("is_string")
                if is_string_attr == "false" and raw_value:
                    params[pname] = _coerce_value(raw_value)
                else:
                    params[pname] = raw_value
            if tool_name:
                blocks.append({
                    "id": f"toolu_{_uuid.uuid4().hex[:24]}",
                    "name": tool_name,
                    "input": params,
                })
            cleaned = cleaned.replace(m.group(0), "")

    # --- Formato 3: <functions><function><name>X</name><params>...</params></function></functions> ---
    if "<function>" in cleaned or "<functions>" in cleaned:
        func_re = re.compile(
            r"<function>\s*<name>(?P<name>[^<]+)</name>\s*<params?>(?P<body>.*?)</params?>\s*</function>",
            re.DOTALL,
        )
        child_re = re.compile(
            r"<(?P<key>[a-z_][a-z0-9_]*)>(?P<val>.*?)</(?P=key)>",
            re.DOTALL,
        )
        for m in func_re.finditer(cleaned):
            tool_name = m.group("name").strip()
            body = m.group("body") or ""
            params = {}
            for cm in child_re.finditer(body):
                params[cm.group("key")] = _coerce_value(cm.group("val").strip())
            if tool_name:
                blocks.append({
                    "id": f"toolu_{_uuid.uuid4().hex[:24]}",
                    "name": tool_name,
                    "input": params,
                })
        cleaned = re.sub(
            r"<functions>\s*(?:<function>.*?</function>\s*)+</functions>",
            "", cleaned, flags=re.DOTALL,
        )
        cleaned = re.sub(r"<function>.*?</function>", "", cleaned, flags=re.DOTALL)

    # --- Formato 2: <tool_name><child_param>value</child_param></tool_name> ---
    if known_tool_names:
        escaped_names = "|".join(re.escape(n) for n in sorted(known_tool_names, key=len, reverse=True))
        tool_tag_re = re.compile(
            rf"<(?P<tname>{escaped_names})\s*>(?P<body>.*?)</(?P=tname)>",
            re.DOTALL,
        )
        child_re = re.compile(
            r"<(?P<key>[a-z_][a-z0-9_]*)>(?P<val>.*?)</(?P=key)>",
            re.DOTALL,
        )
        for m in tool_tag_re.finditer(cleaned):
            tool_name = m.group("tname")
            body = m.group("body") or ""
            params = {}
            for cm in child_re.finditer(body):
                params[cm.group("key")] = _coerce_value(cm.group("val").strip())
            blocks.append({
                "id": f"toolu_{_uuid.uuid4().hex[:24]}",
                "name": tool_name,
                "input": params,
            })
            cleaned = cleaned.replace(m.group(0), "")

    # --- Formato 4: <execute_tool><tool_name>X</tool_name><args>...</args></execute_tool> ---
    # Wrapper esplicito (auto-qualificato): tool_name dichiarato + params dai
    # figli di <args> (o figli diretti se <args> assente).
    if "<execute_tool" in cleaned:
        exec_tool_re = re.compile(
            r"<execute_tool\s*>(?P<body>.*?)</execute_tool\s*>",
            re.DOTALL | re.IGNORECASE,
        )
        name_re = re.compile(
            r"<tool_name\s*>(?P<name>.*?)</tool_name\s*>", re.DOTALL | re.IGNORECASE
        )
        args_re = re.compile(r"<args\s*>(?P<args>.*?)</args\s*>", re.DOTALL | re.IGNORECASE)
        child_re = re.compile(
            r"<(?P<key>[a-z_][a-z0-9_]*)>(?P<val>.*?)</(?P=key)>", re.DOTALL
        )
        for m in exec_tool_re.finditer(cleaned):
            body = m.group("body") or ""
            nm = name_re.search(body)
            if not nm:
                continue
            tool_name = nm.group("name").strip()
            params = {}
            am = args_re.search(body)
            args_body = am.group("args") if am else body
            for cm in child_re.finditer(args_body):
                key = cm.group("key")
                if key == "tool_name":  # evita di risucchiare il tag del nome
                    continue
                params[key] = _coerce_value(cm.group("val").strip())
            if tool_name:
                blocks.append({
                    "id": f"toolu_{_uuid.uuid4().hex[:24]}",
                    "name": tool_name,
                    "input": params,
                })
        cleaned = exec_tool_re.sub("", cleaned)

    # --- Formato 5a: <execute_bash> X(k=v, ...) </execute_bash> ---
    # Wrapper esplicito: il modello "annuncia" una tool-call con sintassi
    # funzione dentro il tag. Auto-qualificato dal wrapper.
    if "<execute_bash" in cleaned:
        exec_bash_re = re.compile(
            r"<execute_bash\s*>(?P<body>.*?)</execute_bash\s*>",
            re.DOTALL | re.IGNORECASE,
        )
        call_re = re.compile(r"(?P<name>[A-Za-z_]\w*)\s*\((?P<args>.*?)\)", re.DOTALL)
        for m in exec_bash_re.finditer(cleaned):
            body = m.group("body") or ""
            cm = call_re.search(body)
            if not cm:
                continue
            tool_name = cm.group("name").strip()
            if not tool_name:
                continue
            params = _parse_function_call_args(cm.group("args"))
            blocks.append({
                "id": f"toolu_{_uuid.uuid4().hex[:24]}",
                "name": tool_name,
                "input": params,
            })
        cleaned = exec_bash_re.sub("", cleaned)

    # --- Formato 5b: function-call NUDA X(k=v, ...) senza wrapper ---
    # Gate anti-falso-positivo: matcha SOLO se il nome e' un tool noto. Cosi'
    # prosa come "config(prod).json" o "uso list_files per..." non matcha.
    if known_tool_names:
        nude_names = "|".join(
            re.escape(n) for n in sorted(known_tool_names, key=len, reverse=True)
        )
        if nude_names:
            nude_call_re = re.compile(
                rf"(?<![\w.])(?P<name>{nude_names})\s*\((?P<args>[^()]*?)\)",
                re.DOTALL,
            )
            for m in nude_call_re.finditer(cleaned):
                tool_name = m.group("name")
                params = _parse_function_call_args(m.group("args"))
                blocks.append({
                    "id": f"toolu_{_uuid.uuid4().hex[:24]}",
                    "name": tool_name,
                    "input": params,
                })
            cleaned = nude_call_re.sub("", cleaned)

    if not blocks:
        return [], text

    # Pulisci wrapper residui
    cleaned = re.sub(r"</?tool_calls\s*/?>", "", cleaned, flags=re.IGNORECASE)
    cleaned = re.sub(r"</?functions\s*/?>", "", cleaned, flags=re.IGNORECASE)
    cleaned = re.sub(r"</?DSML\s*/?>", "", cleaned, flags=re.IGNORECASE)
    cleaned = re.sub(r"</?execute_(?:bash|tool)\s*/?>", "", cleaned, flags=re.IGNORECASE)
    cleaned = re.sub(r"\n{3,}", "\n\n", cleaned).strip()

    return blocks, cleaned
