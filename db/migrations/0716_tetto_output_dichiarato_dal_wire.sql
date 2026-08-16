-- 0716 — Il tetto di output DICHIARATO dal fornitore nel listing di discovery.
--
-- ROOT CAUSE. `fetch_fatti_tetto` legge SOLO `v_model_capabilities`, che nasce
-- `FROM nexus_provider_capabilities LEFT JOIN ai_price_catalog`: un modello
-- scoperto a runtime (che il discovery scrive nel solo lato DESTRO della join)
-- e' invisibile alla vista per costruzione, quindi il suo tetto esce
-- `ModelloNonDichiarato` -> `NonVincolabile` -> nessun `max_tokens` sul wire.
-- Ma per una parte di quei modelli il fornitore il proprio tetto lo DICHIARA
-- gia' nel listing — MISURATO il 16/08/2026 su `GET /v1/models` di openrouter:
-- `data[].top_provider.max_completion_tokens` presente su 364 modelli su 413 —
-- e il parser lo scartava. Il fatto c'era, mancava dove metterlo.
--
-- Provenienza: SOLO il wire (google `outputTokenLimit`, openrouter
-- `top_provider.max_completion_tokens`). NULL = il fornitore non lo dichiara.
-- Mai un default: un tetto inventato produce il turno vuoto fatturato
-- ([[tetto-di-output]] in CLAUDE.md, misurato). Scrittori:
-- `insert_new_chat_model` e `realign_valore_dichiarato`
-- (mcp-core/src/model_catalog_sync.rs). La dichiarazione UMANA resta in
-- `nexus_provider_capabilities` e VINCE su questa colonna
-- (capability.rs::fatti_con_provenienza: il ramo `Presente` non la legge).
--
-- Idempotente.

ALTER TABLE ai_price_catalog
    ADD COLUMN IF NOT EXISTS declared_max_output_tokens integer NULL
    CHECK (declared_max_output_tokens IS NULL OR declared_max_output_tokens > 0);

COMMENT ON COLUMN ai_price_catalog.declared_max_output_tokens IS
    'Tetto di output dichiarato dal fornitore nel listing (wire discovery). NULL = non dichiarato. Scritto solo da model_catalog_sync; la riga di nexus_provider_capabilities, quando esiste, ha precedenza.';

-- La vista espone la colonna IN CODA (CREATE OR REPLACE compatibile: solo
-- colonne finali aggiunte, ordine esistente invariato — stesso vincolo della
-- mig 0478, il cui corpo e' riprodotto qui INTEGRALMENTE). La colonna in vista
-- serve alla coppia CURATA per vedere anche il dichiarato (audit dei mismatch
-- futuri) e a tenere UNA query per il caso `Presente`; il modello NON curato
-- resta fuori dalla vista per costruzione (LEFT JOIN guidata dalla capability),
-- e per lui il criterio legge la colonna direttamente dalla tabella
-- (capability.rs::fetch_dichiarazione_wire).
CREATE OR REPLACE VIEW v_model_capabilities AS
SELECT cap.provider,
       cap.model,
       COALESCE(c.supports_tool_use, true)   AS tool_use,
       COALESCE(c.supports_vision, false)    AS vision,
       COALESCE(c.uses_thinking_mode, false) AS thinking,
       cap.max_context_tokens,
       cap.default_max_output_tokens,
       cap.max_output_tokens_hard,
       cap.tool_choice_style,
       cap.tool_choice_first_turn_force,
       cap.schema_strict,
       cap.schema_dialect,
       cap.tool_call_format,
       cap.max_tools_in_request,
       cap.supports_prompt_cache,
       cap.prompt_cache_dialect,
       cap.supports_parallel_tools,
       cap.stop_reason_dialect,
       cap.soft_failure_iter_threshold,
       cap.soft_failure_content_threshold,
       cap.history_keep_recent_messages,
       cap.history_max_old_tool_result_chars,
       cap.request_timeout_seconds,
       cap.connect_timeout_seconds,
       cap.tool_result_max_chars,
       cap.tool_result_max_bytes,
       cap.tool_result_max_lines,
       COALESCE(c.agentic_thinking_policy, 'none') AS agentic_thinking_policy,
       COALESCE(c.supports_image_gen, false) AS image_gen,
       COALESCE(c.supports_audio_in, false)  AS audio_in,
       COALESCE(c.supports_audio_out, false) AS audio_out,
       COALESCE(c.supports_video_gen, false) AS video_gen,
       c.declared_max_output_tokens
FROM nexus_provider_capabilities cap
LEFT JOIN ai_price_catalog c
       ON c.provider = cap.provider AND c.model = cap.model;
