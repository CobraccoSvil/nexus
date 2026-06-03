---
id: db9b6c76-63b0-41bd-aaf0-901b728c32db
kind: changelog
title: "-- 0258_deepseek_v4_catalog_context_window.sql -- -- Completa il fix della migrazione 0256. La 0256 ha corretto il context window -- dei modelli DeepSeek V4 (deepseek-v4-flash, deepseek-v4-pro) da 8192 a 131072, -- ma SOLO nella tabella nexus_provider_capabilities.max_context_tokens. -- -- Il guard predittivo del brain (_predictive_cap_check in brain/agents/nodes.py) -- legge pero' il context window da ai_price_catalog.context_window, rimasto a -- 8192. Conseguenza: cap predittivo = 50% * 8192 = 4096 token. Il context -- agentico (system prompt + tool + history) supera quasi sempre 4096, quindi -- OGNI tool_call (es. list_files) veniva bloccata da predictive_cap con -- \"[ERROR: chiamata bloccata da predictive context cap]\". Il modello non poteva -- eseguire alcun tool -> hollow completion / loop -> la chat \"non risolveva\". -- -- Fix: allinea ai_price_catalog.context_window a 131072 (context ufficiale -- DeepSeek, gia' verificato empiricamente in 0256 con prompt da ~78.000 token). -- I default/hard output tokens NON c'entrano e restano invariati. -- -- Regola G/H: la verita' resta nel DB via migrazione versionata, niente UPDATE -- ad-hoc fuori migrazione. Idempotente."
slug: 0258-deepseek-v4-catalog-context-windowsql-completa-il-fix-della-migrazione-0256
tags:
  - changelog
source_commit: e6e80d5373c1181056892451780e1d306405e4a5
source_files:
  - db/migrations/0258_deepseek_v4_catalog_context_window.sql
auto_generated: true
created_at: 2026-06-03T07:56:25Z
updated_at: 2026-06-03T07:56:24Z
nexus_meta_version: 1
---

# -- 0258_deepseek_v4_catalog_context_window.sql -- -- Completa il fix della migrazione 0256. La 0256 ha corretto il context window -- dei modelli DeepSeek V4 (deepseek-v4-flash, deepseek-v4-pro) da 8192 a 131072, -- ma SOLO nella tabella nexus_provider_capabilities.max_context_tokens. -- -- Il guard predittivo del brain (_predictive_cap_check in brain/agents/nodes.py) -- legge pero' il context window da ai_price_catalog.context_window, rimasto a -- 8192. Conseguenza: cap predittivo = 50% * 8192 = 4096 token. Il context -- agentico (system prompt + tool + history) supera quasi sempre 4096, quindi -- OGNI tool_call (es. list_files) veniva bloccata da predictive_cap con -- "[ERROR: chiamata bloccata da predictive context cap]". Il modello non poteva -- eseguire alcun tool -> hollow completion / loop -> la chat "non risolveva". -- -- Fix: allinea ai_price_catalog.context_window a 131072 (context ufficiale -- DeepSeek, gia' verificato empiricamente in 0256 con prompt da ~78.000 token). -- I default/hard output tokens NON c'entrano e restano invariati. -- -- Regola G/H: la verita' resta nel DB via migrazione versionata, niente UPDATE -- ad-hoc fuori migrazione. Idempotente.

**Commit**: `e6e80d5373c1181056892451780e1d306405e4a5` (2026-06-03 07:56 UTC)

**Significance**: 0.51

## File toccati

- `db/migrations/0258_deepseek_v4_catalog_context_window.sql`

## Cosa cambia

-- 0258_deepseek_v4_catalog_context_window.sql -- -- Completa il fix della migrazione 0256. La 0256 ha corretto il context window -- dei modelli DeepSeek V4 (deepseek-v4-flash, deepseek-v4-pro) da 8192 a 131072, -- ma SOLO nella tabella nexus_provider_capabilities.max_context_tokens. -- -- Il guard predittivo del brain (_predictive_cap_check in brain/agents/nodes.py) -- legge pero' il context window da ai_price_catalog.context_window, rimasto a -- 8192. Conseguenza: cap predittivo = 50% * 8192 = 4096 token. Il context -- agentico (system prompt + tool + history) supera quasi sempre 4096, quindi -- OGNI tool_call (es. list_files) veniva bloccata da predictive_cap con -- "[ERROR: chiamata bloccata da predictive context cap]". Il modello non poteva -- eseguire alcun tool -> hollow completion / loop -> la chat "non risolveva". -- -- Fix: allinea ai_price_catalog.context_window a 131072 (context ufficiale -- DeepSeek, gia' verificato empiricamente in 0256 con prompt da ~78.000 token). -- I default/hard output tokens NON c'entrano e restano invariati. -- -- Regola G/H: la verita' resta nel DB via migrazione versionata, niente UPDATE -- ad-hoc fuori migrazione. Idempotente.

## Riferimenti

- Vedi diff git: `git show e6e80d5373c1181056892451780e1d306405e4a5`

## Documenti correlati

- [[postgres-tables]]
- [[migrations-log]]
