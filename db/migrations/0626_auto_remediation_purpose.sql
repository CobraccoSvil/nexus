-- 0626_auto_remediation_purpose.sql
-- Purpose 'auto_remediation': i run di rimedio automatico (service_observer,
-- resource_violation) partono da un modello di tier adeguato (heavy:
-- ragionamento per la diagnosi causa-radice + edit di file), non dal default
-- piccolo del routing. Prima i siti passavano provider/model_override = None e
-- il rimedio finiva spesso sul modello piu' debole -> tentativi bruciati
-- ("tenta e fallisce"), osservato sul progetto vendita-immobile.
--
-- Tier-only (regola G): provider/model_id sono placeholder NOT NULL
-- ('__tier_routed__', convenzione mig 0478); il resolver tier-only li IGNORA e
-- sceglie il modello dal catalog live per tier + tool-use (punto unico
-- resolve_purpose_core, regola L). requires_tool_use=true attiva il profilo
-- agentico (require_tool_use + thinking-non-exclude + QualificationGate).
-- required_capability NULL di proposito: un vincolo in piu' che desse
-- NoCapableModel farebbe degradare in silenzio al routing di default,
-- sconfiggendo l'upgrade. Regolabile a caldo senza deploy:
--   UPDATE nexus_purpose_model SET tier='high' WHERE purpose='auto_remediation';
INSERT INTO nexus_purpose_model
    (purpose, provider, model_id, tier, required_capability, requires_tool_use, notes)
VALUES
    ('auto_remediation', '__tier_routed__', '__tier_routed__', 'heavy', NULL, true,
     'Run di rimedio automatico (service_observer, resource_violation): tier heavy, tool-use, tier-only (mig 0626)')
ON CONFLICT (purpose) DO UPDATE SET
    tier = EXCLUDED.tier,
    required_capability = EXCLUDED.required_capability,
    requires_tool_use = EXCLUDED.requires_tool_use,
    notes = EXCLUDED.notes,
    updated_at = NOW();
