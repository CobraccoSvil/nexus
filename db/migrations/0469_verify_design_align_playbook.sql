-- 0469_verify_design_align_playbook.sql
-- FIX (verifica/allineamento al design Figma su progetto ESISTENTE): un task tipo
-- "allinea il layout al design Figma" su un progetto gia' creato non pianificava MAI
-- nexus_visual_compare, perche' l'unico playbook che lo inietta (implement.figma_make,
-- mig 0468) richiede un allegato .make (trigger attachment_kind=figma_make +
-- project_markers=figma_export), assente sui progetti esistenti. Senza visual_compare
-- nella history non nasce alcun similarity_score, quindi il gate final_gate
-- design_verify (mig 0467 + final_gate.rs build_criteria) non viene nemmeno COSTRUITO:
-- l'agente "allinea alla cieca" e chiude senza confronto visivo (incidente Beauty-Book).
--
-- Fix definitivo al PUNTO UNICO (playbook engine, regole G + L): un nuovo playbook
-- verify.design_align che pianifica deterministicamente nexus_visual_compare per i task
-- di verifica/allineamento al design, SENZA richiedere un allegato .make. Il passo e' lo
-- stesso (riuso testuale) gia' presente in implement.figma_make (mig 0468). La soglia e
-- il modello vision restano DB-driven (mig 0214/0467); il gate design_verify resta il
-- floor. Niente loop hardcoded nel codice: l'iterazione resta guidata dall'agente in
-- modalita' Continuo; qui si garantisce solo che il passo venga PIANIFICATO e che il
-- gate design_verify possa scattare.
--
-- Trigger DISGIUNTO da implement.figma_make: nessun attachment_kind / project_markers,
-- e keywords specifiche di verifica visiva. Cosi' i due playbook NON si sovrappongono
-- (figma_make solo al bootstrap con .make; design_align sulla verifica di un progetto
-- gia' esistente). priority 110 (l'engine ordina priority DESC) per preferire questo
-- playbook nel caso raro in cui entrambi dovessero matchare con keyword di verifica.
--
-- Idempotente: ON CONFLICT (key) DO NOTHING.
INSERT INTO nexus_task_playbooks (key, title, description, trigger_json, guidance_text, category, priority, steps_json)
VALUES (
  'verify.design_align',
  'Verifica e allineamento al design Figma',
  'Allinea il layout di un progetto esistente al design Figma di riferimento, con verifica visiva deterministica via nexus_visual_compare.',
  '{
     "intent": ["fix", "fix_semplice", "fix_complesso", "refactor", "implement", "frontend", "code", "agentic_default"],
     "keywords": ["allinea il layout", "allinea al design", "conforme al design", "verifica il layout", "confronta col figma", "confronta con il figma", "rispetta il design", "rispetta il figma", "align to design", "pixel perfect", "resa visiva", "layout figma", "design figma", "non rispetta il design", "non rispetta il figma"]
   }'::jsonb,
  'Per i task di verifica o allineamento al design: confronta SEMPRE la resa attuale del frontend con il design Figma usando nexus_visual_compare PRIMA di considerare il task concluso. Itera correggendo solo stile, layout, spaziature, palette e tipografia finche la resa visiva e conforme al design di riferimento.',
  'verification',
  110,
  '["VERIFICA VISIVA col design Figma: esegui nexus_visual_compare(url del frontend avviato, reference = design Figma del progetto) e confronta la resa con il design. Se similarity_score e'' sotto la soglia, correggi SOLO stile/layout/spaziature/palette/tipografia/componenti per avvicinarti al design e ri-esegui nexus_visual_compare, ITERANDO finche'' la resa corrisponde al figma. NON considerare il task completo finche'' la resa visiva non e'' conforme al design."]'::jsonb
)
ON CONFLICT (key) DO NOTHING;
