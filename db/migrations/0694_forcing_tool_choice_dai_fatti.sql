-- 0694: il gate chiede al catalogo se puo' forzare la tool call, e il catalogo
-- dice il vero su mistral.
--
-- ROOT CAUSE (misurata il 09-10/08/2026 su vetrina-statica). Il gate duale
-- scriveva `force_tool_choice: Some(true)` a mano in `richiesta_verdetto`
-- (mcp-core/src/agent_graph_adapter/step_validation.rs) senza mai interrogare
-- il punto unico che risponde a quella domanda — `capability::
-- resolve_tool_choice_style` piu' `provider_style_supports_forcing` — che
-- l'ESECUTORE interrogava gia'. Il codice ora delega; questa migrazione
-- sistema il DATO su cui la delega poggia, dove l'esercizio lo smentisce.
--
-- COSA DICEVANO I FATTI. Sui verdetti persistiti in `nexus_agent_meta_steps`
-- (kind='step_validation') del progetto vetrina-statica:
--   kimi/kimi-k2.6              openai_auto      22 astensioni, 0 verdetti,
--                                                causa client_error (HTTP 400
--                                                "tool_choice required is
--                                                incompatible with thinking
--                                                enabled")
--   mistral/magistral-small-latest openai_auto   10 verdetti ESPRESSI
--                                                (9 approve + 1 reject),
--                                                zero client_error
--   openrouter/z-ai/glm-4.7-flash  (assente)     18 verdetti espressi
--
-- Su kimi il catalogo aveva ragione e il codice torto: la delega da sola
-- chiude quel caso. Su mistral e' il catalogo a essere smentito — quel modello
-- ACCETTA `tool_choice: "required"`, e lo ha dimostrato dieci volte. Lasciarlo
-- `openai_auto` avrebbe spento il forcing sull'UNICO giudice che oggi risponde
-- sempre, cioe' avrebbe pagato la chiusura di un difetto con l'apertura di un
-- altro.
--
-- PERCHE' UNA MIGRAZIONE E NON UN RIPIEGO NEL CODICE. Il campo non e' scritto
-- da nessun sync: nessun UPDATE su `nexus_provider_capabilities.
-- tool_choice_style` esiste nei sorgenti, il valore viene dalle migrazioni.
-- Correggerlo qui sopravvive a un riavvio, a un deploy e a un wipe del DB con
-- ri-applicazione (regola H punto 2). Un'eccezione per mistral scritta in
-- `default_style_for_provider` sarebbe invece la toppa: il codice
-- contraddirebbe il catalogo, e la prossima coppia smentita dai fatti
-- richiederebbe una seconda eccezione.
--
-- PORTATA DELIBERATAMENTE STRETTA. Si corregge SOLO la coppia per cui esiste la
-- prova in esercizio. `devstral-small-2507` e' anch'esso `openai_auto` e non
-- viene toccato: nessuna convocazione lo ha mai usato, quindi non si sa, e
-- riallinearlo "per simmetria di famiglia" sarebbe indovinare — esattamente
-- cio' che il ripiego per famiglia fa gia' quando la riga MANCA, ma qui la riga
-- c'e' e afferma qualcosa.
--
-- NON TOCCA KIMI: quelle righe dicono il vero e restano.

UPDATE nexus_provider_capabilities
   SET tool_choice_style = 'openai_required',
       updated_at = NOW()
 WHERE provider = 'mistral'
   AND model = 'magistral-small-latest'
   AND tool_choice_style = 'openai_auto';

-- Guard: la riga corretta deve esistere e dire cio' che i fatti dicono. Se il
-- modello sparisse dal catalogo la migrazione passerebbe a vuoto, e il gate
-- ricadrebbe sul ripiego per famiglia ('openai_required' per mistral), che
-- porta alla stessa conclusione: e' un'assenza tollerabile, e la si dichiara
-- invece di farla fallire.
DO $$
DECLARE
  v_stile TEXT;
  v_kimi  INT;
BEGIN
  SELECT tool_choice_style INTO v_stile
    FROM nexus_provider_capabilities
   WHERE provider = 'mistral' AND model = 'magistral-small-latest';

  IF v_stile IS NULL THEN
    RAISE NOTICE '0694: mistral/magistral-small-latest non e'' a catalogo; il gate usera'' il ripiego per famiglia (openai_required).';
  ELSIF v_stile <> 'openai_required' THEN
    RAISE EXCEPTION '0694: atteso openai_required per mistral/magistral-small-latest, trovato %', v_stile;
  END IF;

  SELECT COUNT(*) INTO v_kimi
    FROM nexus_provider_capabilities
   WHERE provider = 'kimi' AND tool_choice_style = 'openai_auto';

  IF v_kimi = 0 THEN
    RAISE WARNING '0694: nessun modello kimi resta openai_auto. Se e'' stato riallineato altrove, il gate tornera'' a forzare su un fornitore che risponde 400.';
  ELSE
    RAISE NOTICE '0694: % modelli kimi restano openai_auto, ed e'' corretto: su quelli il forcing non si puo'' usare.', v_kimi;
  END IF;
END $$;
