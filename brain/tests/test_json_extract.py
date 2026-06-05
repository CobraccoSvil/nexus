"""Test del punto unico di estrazione JSON (brain/utils/json_extract.py)."""
from brain.utils.json_extract import extract_json_block


def test_json_puro():
    assert extract_json_block('{"a": 1}') == {"a": 1}


def test_fence_markdown_json():
    assert extract_json_block('```json\n{"a": 1}\n```') == {"a": 1}


def test_fence_markdown_semplice():
    assert extract_json_block('```\n{"a": 1}\n```') == {"a": 1}


def test_testo_prima_e_dopo():
    assert extract_json_block('Ecco il risultato: {"a": 1}. Fine.') == {"a": 1}


def test_oggetto_annidato():
    assert extract_json_block('{"a": {"b": {"c": 2}}}') == {"a": {"b": {"c": 2}}}


def test_graffa_in_stringa_non_confonde():
    assert extract_json_block('{"msg": "ha una } dentro"}') == {"msg": "ha una } dentro"}


def test_nessun_json_ritorna_none():
    assert extract_json_block("nessun oggetto qui") is None


def test_stringa_vuota_ritorna_none():
    assert extract_json_block("") is None


def test_lista_top_level_non_e_dict():
    assert extract_json_block("[1, 2, 3]") is None
