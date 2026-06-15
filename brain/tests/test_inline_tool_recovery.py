"""Parser di recupero tool-as-text: parse_inline_tool_invocations.

Root cause (osservato sulla UI Nexus su task reali): quando il context e'
ingombro, alcuni modelli emettono le tool-call come TESTO nel content invece
che come tool_calls strutturate, p.es.:

    <execute_bash> list_files(path="./") </execute_bash>
    <execute_tool><tool_name>read_file</tool_name><args><path>x</path></args></execute_tool>

Il sistema non le riconosceva e abortiva con "modello non risponde con azione".
parse_inline_tool_invocations (punto unico, regola L, condiviso da tutti i
provider OpenAI-compatible via parse_openai_compatible_choice) ora le recupera.

Questi test coprono: i 3 formati storici (regressione), i 2 nuovi formati
(execute_tool, execute_bash + function-call), e soprattutto l'assenza di falsi
positivi su prosa che menziona un tool senza chiamarlo.
"""
from brain.providers._schema_utils import (
    _parse_function_call_args,
    parse_inline_tool_invocations,
)

KNOWN = {"read_file", "list_files", "edit_file", "run_command"}


# --- Nuovo Formato 5a: <execute_bash> tool(kwargs) </execute_bash> -------------
def test_execute_bash_single_string_arg():
    text = '<execute_bash> list_files(path="./") </execute_bash>'
    blocks, cleaned = parse_inline_tool_invocations(text, KNOWN)
    assert len(blocks) == 1
    assert blocks[0]["name"] == "list_files"
    assert blocks[0]["input"] == {"path": "./"}
    assert "execute_bash" not in cleaned


def test_execute_bash_bool_args():
    text = "<execute_bash> list_files(recursive=True, long_format=False) </execute_bash>"
    blocks, _ = parse_inline_tool_invocations(text, KNOWN)
    assert len(blocks) == 1
    assert blocks[0]["name"] == "list_files"
    assert blocks[0]["input"] == {"recursive": True, "long_format": False}


def test_execute_bash_works_even_without_known_names():
    # Il wrapper esplicito qualifica la chiamata anche senza whitelist.
    text = '<execute_bash> some_new_tool(x="y") </execute_bash>'
    blocks, _ = parse_inline_tool_invocations(text, None)
    assert len(blocks) == 1
    assert blocks[0]["name"] == "some_new_tool"
    assert blocks[0]["input"] == {"x": "y"}


# --- Nuovo Formato 4: <execute_tool><tool_name>X</tool_name><args>...</args> ----
def test_execute_tool_with_args():
    text = (
        "<execute_tool><tool_name>read_file</tool_name>"
        "<args><path>src/app/types/index.ts</path></args></execute_tool>"
    )
    blocks, cleaned = parse_inline_tool_invocations(text, KNOWN)
    assert len(blocks) == 1
    assert blocks[0]["name"] == "read_file"
    assert blocks[0]["input"] == {"path": "src/app/types/index.ts"}
    assert "execute_tool" not in cleaned


def test_execute_tool_with_whitespace_and_newlines():
    text = (
        "<execute_tool>\n  <tool_name>read_file</tool_name>\n"
        "  <args>\n    <path>config.json</path>\n  </args>\n</execute_tool>"
    )
    blocks, _ = parse_inline_tool_invocations(text, KNOWN)
    assert len(blocks) == 1
    assert blocks[0]["name"] == "read_file"
    assert blocks[0]["input"] == {"path": "config.json"}


# --- Nuovo Formato 5b: function-call nuda (gate known_tool_names) --------------
def test_nude_function_call_known_tool():
    text = 'Procedo. read_file(path="package.json")'
    blocks, _ = parse_inline_tool_invocations(text, KNOWN)
    assert len(blocks) == 1
    assert blocks[0]["name"] == "read_file"
    assert blocks[0]["input"] == {"path": "package.json"}


def test_nude_function_call_unknown_tool_is_ignored():
    # Nome non in whitelist + nessun wrapper -> NON deve essere una tool-call.
    text = 'parse_config(path="x")'
    blocks, cleaned = parse_inline_tool_invocations(text, KNOWN)
    assert blocks == []
    assert cleaned == text


# --- Anti-falsi-positivi (critico) --------------------------------------------
def test_prose_mentioning_tool_no_parens():
    text = "Userò list_files per esplorare la cartella e poi read_file sui sorgenti."
    blocks, cleaned = parse_inline_tool_invocations(text, KNOWN)
    assert blocks == []
    assert cleaned == text


def test_prose_with_parens_but_not_a_tool():
    text = "Il file config(prod).json contiene la chiave; vedi anche backup(old)."
    blocks, cleaned = parse_inline_tool_invocations(text, KNOWN)
    assert blocks == []
    assert cleaned == text


def test_plain_text_unchanged():
    text = "Questo è un normale paragrafo di risposta senza alcuna tool-call."
    blocks, cleaned = parse_inline_tool_invocations(text, KNOWN)
    assert blocks == []
    assert cleaned == text


# --- Regressione: i 3 formati storici restano intatti -------------------------
def test_format1_invoke_still_works():
    text = '<invoke name="read_file"><parameter name="path">a.txt</parameter></invoke>'
    blocks, _ = parse_inline_tool_invocations(text, KNOWN)
    assert len(blocks) == 1
    assert blocks[0]["name"] == "read_file"
    assert blocks[0]["input"] == {"path": "a.txt"}


def test_format2_tool_tag_still_works():
    text = "<read_file><path>b.txt</path></read_file>"
    blocks, _ = parse_inline_tool_invocations(text, KNOWN)
    assert len(blocks) == 1
    assert blocks[0]["name"] == "read_file"
    assert blocks[0]["input"] == {"path": "b.txt"}


def test_format3_functions_still_works():
    text = (
        "<functions><function><name>list_files</name>"
        "<params><path>./</path></params></function></functions>"
    )
    blocks, _ = parse_inline_tool_invocations(text, KNOWN)
    assert len(blocks) == 1
    assert blocks[0]["name"] == "list_files"
    assert blocks[0]["input"] == {"path": "./"}


# --- Helper _parse_function_call_args -----------------------------------------
def test_parse_args_mixed_types():
    out = _parse_function_call_args('path="./src", recursive=True, depth=3, ratio=0.5')
    assert out == {"path": "./src", "recursive": True, "depth": 3, "ratio": 0.5}


def test_parse_args_string_with_comma():
    out = _parse_function_call_args('msg="ciao, mondo", n=1')
    assert out == {"msg": "ciao, mondo", "n": 1}


def test_parse_args_empty():
    assert _parse_function_call_args("") == {}
    assert _parse_function_call_args("   ") == {}


def test_parse_args_none_literal():
    out = _parse_function_call_args("value=None, flag=False")
    assert out == {"value": None, "flag": False}


# --- Combinazioni -------------------------------------------------------------
def test_text_around_execute_tool_is_preserved():
    text = (
        "Ok, procedo con la lettura.\n"
        "<execute_tool><tool_name>read_file</tool_name>"
        "<args><path>x.ts</path></args></execute_tool>\n"
        "Poi analizzo il risultato."
    )
    blocks, cleaned = parse_inline_tool_invocations(text, KNOWN)
    assert len(blocks) == 1
    assert "procedo con la lettura" in cleaned
    assert "analizzo il risultato" in cleaned
    assert "execute_tool" not in cleaned


def test_multiple_execute_bash_calls():
    text = (
        '<execute_bash> read_file(path="a.ts") </execute_bash>\n'
        '<execute_bash> read_file(path="b.ts") </execute_bash>'
    )
    blocks, _ = parse_inline_tool_invocations(text, KNOWN)
    assert len(blocks) == 2
    assert {b["input"]["path"] for b in blocks} == {"a.ts", "b.ts"}
