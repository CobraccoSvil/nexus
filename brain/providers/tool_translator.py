"""Traduzione dei tool canonici nel dialetto di ogni provider (M1 del piano).

I tool Nexus sono definiti UNA volta in formato canonico (CanonicalTool, schema
stile Anthropic). Questo modulo li traduce nel dialetto del provider scelto dalle
capability DB (schema_dialect + tool_call_format), senza che il resto del sistema
sappia che esistono dialetti diversi.

Cinque dialetti:
- AnthropicDialect        -> tool blocks nativi con input_schema
- OpenAIStrictDialect     -> tools[] con function.parameters (strict=true)
- OpenAILooseDialect      -> tools[] con function.parameters (Mistral-friendly)
- GoogleDialect           -> FunctionDeclaration con parameters sanitizzati
- TextFallbackDialect     -> nessun payload nativo; tool descritti nel system
                             prompt + parsing XML inline (vLLM/Ollama tool-mute)

Riusa _schema_utils (compress_schema, compress_tool_list, _truncate_text): non
duplica la logica di compressione gia esistente. La selezione del dialetto e
guidata dal DB (regola G), non da if hardcoded nei provider.
"""
from __future__ import annotations

import abc
from typing import Any

from ._models import CanonicalTool, ProviderCapability
from ._schema_utils import (
    DEFAULT_TOOL_DESCR_MAX,
    _truncate_text,
    compress_schema,
)


class ToolDialect(abc.ABC):
    """Strategia di traduzione tool per una famiglia di provider."""

    name: str = "base"
    #: True se il provider riceve i tool come payload nativo; False se tool-mute
    #: (i tool vanno descritti nel system prompt).
    native: bool = True

    @abc.abstractmethod
    def translate_tools(
        self, tools: list[CanonicalTool], cap: ProviderCapability
    ) -> list[dict] | None:
        """Payload tool nel dialetto del provider, o None se tool-mute."""
        ...

    def documentation_block(self, tools: list[CanonicalTool]) -> str:
        """Descrizione testuale dei tool per il system prompt.

        Default: vuoto per dialetti nativi (i tool arrivano come payload). Il
        TextFallbackDialect lo sovrascrive per i provider tool-mute.
        """
        return ""

    @staticmethod
    def _apply_tool_cap(tools: list[CanonicalTool], cap: ProviderCapability) -> list[CanonicalTool]:
        """Limita il numero di tool se la capability impone un cap (es. o-series)."""
        if cap.max_tools_in_request and cap.max_tools_in_request > 0:
            return tools[: cap.max_tools_in_request]
        return tools


class AnthropicDialect(ToolDialect):
    name = "anthropic"

    def translate_tools(self, tools, cap):
        tools = self._apply_tool_cap(tools, cap)
        out: list[dict] = []
        for t in tools:
            out.append({
                "name": t.name,
                "description": _truncate_text(t.description, DEFAULT_TOOL_DESCR_MAX),
                "input_schema": compress_schema(t.input_schema) if t.input_schema else {"type": "object"},
            })
        return out


class _OpenAIBase(ToolDialect):
    strict: bool = False

    def translate_tools(self, tools, cap):
        tools = self._apply_tool_cap(tools, cap)
        out: list[dict] = []
        for t in tools:
            params = compress_schema(t.input_schema) if t.input_schema else {"type": "object", "properties": {}}
            fn: dict[str, Any] = {
                "name": t.name,
                "description": _truncate_text(t.description, DEFAULT_TOOL_DESCR_MAX),
                "parameters": params,
            }
            # strict mode (OpenAI) richiede additionalProperties:false e tutti i
            # campi in required; lo attiviamo solo per il dialetto strict.
            if self.strict or cap.schema_strict:
                fn["strict"] = True
                if isinstance(params, dict) and params.get("type") == "object":
                    params.setdefault("additionalProperties", False)
            out.append({"type": "function", "function": fn})
        return out


class OpenAIStrictDialect(_OpenAIBase):
    name = "openai_strict"
    strict = True


class OpenAILooseDialect(_OpenAIBase):
    name = "openai_loose"
    strict = False


class GoogleDialect(ToolDialect):
    name = "google_function_declaration"

    def translate_tools(self, tools, cap):
        tools = self._apply_tool_cap(tools, cap)
        decls: list[dict] = []
        for t in tools:
            params = compress_schema(t.input_schema) if t.input_schema else {"type": "object"}
            decls.append({
                "name": t.name,
                "description": _truncate_text(t.description, DEFAULT_TOOL_DESCR_MAX),
                "parameters": params,
            })
        # Google raggruppa le FunctionDeclaration sotto un unico tool.
        return [{"function_declarations": decls}]


class TextFallbackDialect(ToolDialect):
    """Per provider tool-mute (vLLM/Ollama): nessun payload nativo, i tool sono
    descritti nel system prompt e il modello emette tool-call come XML inline,
    poi recuperati da _schema_utils.parse_inline_tool_invocations."""

    name = "text_fallback"
    native = False

    def translate_tools(self, tools, cap):
        return None

    def documentation_block(self, tools: list[CanonicalTool]) -> str:
        if not tools:
            return ""
        lines: list[str] = [
            "## Strumenti disponibili",
            "",
            "Per usare uno strumento, emetti ESATTAMENTE questo formato XML "
            "(uno o piu blocchi), senza altro testo attorno:",
            "",
            '<invoke name="NOME_STRUMENTO">',
            '<parameter name="NOME_PARAMETRO">valore</parameter>',
            "</invoke>",
            "",
            "Strumenti:",
        ]
        for i, t in enumerate(tools, 1):
            desc = _truncate_text(t.description, DEFAULT_TOOL_DESCR_MAX)
            lines.append(f"{i}. **{t.name}** - {desc}")
            props = (t.input_schema or {}).get("properties") or {}
            required = set((t.input_schema or {}).get("required") or [])
            for pname, pinfo in props.items():
                ptype = pinfo.get("type", "any") if isinstance(pinfo, dict) else "any"
                pdesc = pinfo.get("description", "") if isinstance(pinfo, dict) else ""
                req = " (obbligatorio)" if pname in required else ""
                pdesc_s = f" - {_truncate_text(pdesc, 120)}" if pdesc else ""
                lines.append(f"   - `{pname}` ({ptype}){req}{pdesc_s}")
        return "\n".join(lines)


_DIALECT_BY_NAME: dict[str, ToolDialect] = {
    "anthropic": AnthropicDialect(),
    "openai_strict": OpenAIStrictDialect(),
    "openai_loose": OpenAILooseDialect(),
    "google_function_declaration": GoogleDialect(),
    "text_fallback": TextFallbackDialect(),
}


def get_dialect(name: str) -> ToolDialect:
    """Dialetto per nome (schema_dialect o 'text_fallback'). KeyError se ignoto."""
    d = _DIALECT_BY_NAME.get(name)
    if d is None:
        raise KeyError(
            f"Dialetto tool sconosciuto: '{name}'. "
            f"Attesi: {sorted(_DIALECT_BY_NAME)}."
        )
    return d


def dialect_for_capability(cap: ProviderCapability) -> ToolDialect:
    """Sceglie il dialetto in base alle capability DB.

    Se il formato tool-call e 'xml_inline_fallback' (provider tool-mute) usa il
    TextFallbackDialect; altrimenti usa schema_dialect. Regola G: la scelta viene
    dal DB, mai hardcoded nel provider.
    """
    if cap.tool_call_format == "xml_inline_fallback":
        return _DIALECT_BY_NAME["text_fallback"]
    return get_dialect(cap.schema_dialect)
