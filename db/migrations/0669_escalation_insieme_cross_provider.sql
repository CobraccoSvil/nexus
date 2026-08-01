-- Migrazione 0669 — L'escalation cross-provider sceglie da un INSIEME.
--
-- Aggiunge `agent.escalation.cross_provider_candidates_per_tier` (default 3):
-- quanti candidati cross-provider l'escalation tiene PER LIVELLO di capacita'
-- (tier), oltre alla preferenza dichiarata dal purpose `loop_fallback_default`.
--
-- Trigger: run cfb781ff-ca2a-4bb3-9a27-fb5e5cfadf96 su bacheca-attivita
-- (01/08/2026). Il final_gate boccia due cicli e chiede l'escalation; il run
-- chiude failed_diagnosed con "catena di escalation esaurita. Interrompo".
-- Il motivo: il tier 2 era un ripiego SINGOLO. La catena intra-provider era
-- vuota (deepseek/deepseek-v4-pro e' il modello con `escalation_rank` piu' alto
-- del suo fornitore) e l'unico candidato cross-provider — il purpose, risolto
-- su una finestra da 131k contro il milione del corrente — veniva scartato dal
-- guard downgrade-finestra, correttamente. Nello stesso istante il catalogo
-- aveva sedici modelli abilitati con finestra >= 1M su tre fornitori diversi,
-- tutti con chiave configurata: "non ho alternative" era DEDOTTO da una lista
-- di uno.
--
-- Il tetto e' PER LIVELLO e non globale: il pool eleggibile arriva ordinato per
-- NON-thinking, poi costo crescente, poi featured (l'ordine con cui si sceglie
-- un SOSTITUTO, non il piu' capace: i modelli capaci stanno in fondo perche'
-- costano). Un tetto globale su
-- quell'ordine escluderebbe sistematicamente i livelli alti — lo stesso difetto
-- gia' misurato sul backstop del pool di failover, dove un limite 64 su ~112
-- eleggibili tagliava tutti i frontier/heavy. Con un tetto per livello ogni
-- banda resta rappresentata e il cap "un solo gradino di tier" a valle trova
-- qualcosa da cui scegliere invece di una banda vuota.
--
-- `0` disattiva l'insieme: resta la sola preferenza dichiarata dal purpose,
-- cioe' il comportamento precedente a questa migrazione.

INSERT INTO settings (key, value, category, description, updated_at) VALUES
    ('agent.escalation.cross_provider_candidates_per_tier', '3', 'agent',
     'Quanti candidati cross-provider l''auto-escalation tiene per ciascun livello di capacita'' (tier), oltre alla preferenza dichiarata dal purpose loop_fallback_default. Entrano solo i candidati almeno capaci quanto il modello corrente (duale di escalation_rank > corrente della catena intra-provider) e con finestra di contesto non inferiore. 0 disattiva l''insieme e lascia la sola preferenza. Default 3.',
     NOW())
ON CONFLICT (key) DO NOTHING;
