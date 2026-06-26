#!/usr/bin/env python3
"""Golden di parita' 1:1 per `decisions::escalation::pick_escalation_model`
(SELEZIONE del modello di auto-escalation) del crate `nexus-agent-graph`.

Replica la SELEZIONE di `_pick_escalation_model`
(`brain/agents/nodes/helpers.py:1702-1760`), usata sia dalla loop-detection per
signature (`brain/agents/nodes/__init__.py:3159-3284`) sia dal cap G1
(`__init__.py:1962-1993`):

  # Tier 1: catena intra-provider (stesso provider, tier superiore)
  if provider and model and provider.strip().lower() not in cooldown_set:
      rows = SELECT escalation_model FROM nexus_model_escalation_chain
             WHERE provider=? AND base_model=? AND is_active
             ORDER BY escalation_position ASC LIMIT escalations+1
      if rows and len(rows) > escalations:
          cand = rows[escalations]
          if cand and cand != model:
              return (provider, cand)
  # Tier 2: purpose model cross-provider (loop_fallback_default) dal router
  d = router.purpose_model("loop_fallback_default")
  if d.provider not in SENTINELS and not (d.provider == provider and d.model == model):
      return (d.provider, d.model)
  return None

La funzione REALE fa I/O (DB per la catena, gate cooldown, router per il
cross-provider): NON e' invocabile in modo puro con (catena, cooldown, cross) come
parametri. La SELEZIONE in se' e' deterministica/booleana: la riproduciamo qui 1:1
(come gen_golden_executor_g1.py per il conteggio G1 embedded), che e' la fonte di
verita' del comportamento Python osservabile. Gli input modellano cio' che l'impl
della porta Rust risolve a monte:
  - chain: lista ordinata di escalation_model gia' filtrata (is_active) per (provider, base_model)
  - provider_in_cooldown: bool (provider corrente in cooldown -> salta Tier 1)
  - cross_provider: [provider, model] del loop_fallback_default (sentinelle gia' escluse), o None

Output: /tmp/golden_escalation.json — lista di {group, case_id, input, output}.
Output Python: [provider, model] o null (from_chain NON e' osservabile nel ritorno
Python `tuple[str, str] | None`, quindi non entra nel golden).

Uso:
  python3 crates/nexus-agent-graph/scripts/gen_golden_escalation.py
  cargo test -p nexus-agent-graph --lib golden_escalation -- --ignored
"""
from __future__ import annotations

import json
import os
import sys

_REPO_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))
if _REPO_ROOT not in sys.path:
    sys.path.insert(0, _REPO_ROOT)


def pick_escalation_model(
    chain,
    current_provider,
    current_model,
    escalations,
    provider_in_cooldown,
    cross_provider,
):
    """Replica 1:1 della SELEZIONE di _pick_escalation_model."""
    # Tier 1: catena intra-provider (stesso provider, tier superiore).
    if current_provider and current_model and not provider_in_cooldown:
        # `_rows[escalations]` con `LIMIT escalations+1`: indice = escalation gia' fatte.
        if 0 <= escalations < len(chain):
            cand = chain[escalations]
            if cand and cand != current_model:
                return [current_provider, cand]
    # Tier 2: purpose cross-provider (loop_fallback_default).
    if cross_provider is not None:
        cp_provider, cp_model = cross_provider[0], cross_provider[1]
        same_as_current = cp_provider == current_provider and cp_model == current_model
        if not same_as_current:
            return [cp_provider, cp_model]
    return None


def main() -> None:
    cases = []
    # (case_id, chain, provider, model, escalations, cooldown, cross)
    inputs = [
        # Tier 1 catena, prima posizione.
        ("tier1_prima_posizione",
         ["claude-sonnet-4-6", "claude-opus-4-6"], "anthropic", "claude-haiku-4-5", 0, False, None),
        # Tier 1 indice segue escalations.
        ("tier1_indice_segue_escalations",
         ["claude-sonnet-4-6", "claude-opus-4-6"], "anthropic", "claude-haiku-4-5", 1, False, None),
        # Tier 1 catena esaurita, nessun cross -> None.
        ("tier1_catena_esaurita_none",
         ["claude-sonnet-4-6"], "anthropic", "claude-haiku-4-5", 1, False, None),
        # Tier 1 candidato == corrente -> salta al cross.
        ("tier1_candidato_uguale_va_cross",
         ["claude-haiku-4-5"], "anthropic", "claude-haiku-4-5", 0, False, ["google", "gemini-2.5-pro"]),
        # Provider in cooldown salta Tier 1, va al cross.
        ("cooldown_salta_tier1_va_cross",
         ["claude-sonnet-4-6"], "anthropic", "claude-haiku-4-5", 0, True, ["openai", "gpt-4.1"]),
        # Provider in cooldown senza cross -> None.
        ("cooldown_senza_cross_none",
         ["claude-sonnet-4-6"], "anthropic", "claude-haiku-4-5", 0, True, None),
        # Nessun provider corrente -> salta Tier 1, usa cross.
        ("no_provider_corrente_usa_cross",
         ["claude-sonnet-4-6"], None, None, 0, False, ["mistral", "mistral-large-2411"]),
        # Cross == corrente -> None.
        ("cross_uguale_corrente_none",
         [], "anthropic", "claude-haiku-4-5", 0, False, ["anthropic", "claude-haiku-4-5"]),
        # Catena vuota usa cross.
        ("catena_vuota_usa_cross",
         [], "openai", "gpt-4o-mini", 0, False, ["google", "gemini-2.5-flash"]),
        # Tutto assente -> None.
        ("tutto_assente_none",
         [], "openai", "gpt-4o-mini", 0, False, None),
        # Catena reale OpenAI (seed mig 0128), escalations=0.
        ("openai_seed_pos0",
         ["gpt-4.1-mini", "gpt-4.1", "o4-mini", "o3"], "openai", "gpt-4o-mini", 0, False, None),
        # Catena reale OpenAI, escalations=2 (3a posizione).
        ("openai_seed_pos2",
         ["gpt-4.1-mini", "gpt-4.1", "o4-mini", "o3"], "openai", "gpt-4o-mini", 2, False, None),
        # Catena reale, escalations oltre la fine, cross presente.
        ("oltre_catena_usa_cross",
         ["gpt-4.1-mini", "gpt-4.1"], "openai", "gpt-4o-mini", 5, False, ["deepseek", "deepseek-reasoner"]),
    ]
    for (cid, chain, prov, model, esc, cooldown, cross) in inputs:
        out = pick_escalation_model(chain, prov, model, esc, cooldown, cross)
        cases.append({
            "group": "pick_escalation_model",
            "case_id": cid,
            "input": {
                "chain": chain,
                "current_provider": prov,
                "current_model": model,
                "escalations": esc,
                "provider_in_cooldown": cooldown,
                "cross_provider": cross,
            },
            "output": out,
        })

    out_path = "/tmp/golden_escalation.json"
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(cases, f, ensure_ascii=False, indent=2)
    print(f"golden escalation: {len(cases)} casi scritti in {out_path}")


if __name__ == "__main__":
    main()
