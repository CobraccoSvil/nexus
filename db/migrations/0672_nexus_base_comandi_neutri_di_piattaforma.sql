-- 0672_nexus_base_comandi_neutri_di_piattaforma.sql
--
-- `system.nexus_base` insegnava comandi solo-Linux FUORI da <privilegi_sistema>
-- (l'unico blocco che il gate d'ambiente rimuove su assenza accertata): `ss
-- -tlnp` per le porte in ascolto (righe 31 e 336) e `lsof -t | xargs kill -9`
-- per la porta occupata (riga 34). Misurato il 02/08/2026 (sub-run a5f7419c,
-- figura verify): 180 secondi e 16 iterazioni a scoprire per tentativi che
-- l'host non e' Linux — il blocco <ambiente_esecuzione> ora DICHIARA l'host
-- (mig 0670), ma queste righe continuavano a prescrivere l'attrezzo sbagliato
-- con l'autorita' del system prompt.
--
-- La riga 34 era sbagliata DUE volte: `lsof` non esiste su Windows, e "porta
-- occupata -> kill -9 dell'occupante" contraddice l'isolamento dei progetti
-- (CLAUDE.md sez. E): l'occupante puo' essere un servizio di un ALTRO progetto,
-- e la rimozione forzata e' compito dell'enforcer, mai dell'agente. Il rimedio
-- giusto esiste gia' ed e' request_port.
--
-- Il criterio del fix: il template non prescrive l'attrezzo di UN sistema
-- operativo; rimanda al fatto dichiarato nel blocco ambiente e da' l'attrezzo
-- per ciascun host dove serve un esempio. Niente nuova euristica nel codice:
-- e' testo per il modello, corretto nel posto dove vive (regola G).

UPDATE nexus_prompt_templates SET content = replace(content,
  '4) Dopo che i servizi sono avviati, VERIFICA con run_command("ss -tlnp | grep PORTA") che le porte siano in ascolto.',
  '4) Dopo che i servizi sono avviati, VERIFICA con run_command che le porte siano in ascolto, con l''attrezzo dell''host dichiarato in <ambiente_esecuzione>: su Linux `ss -tlnp | grep :PORTA`, su Windows `netstat -ano | findstr :PORTA`.')
WHERE key = 'system.nexus_base';

UPDATE nexus_prompt_templates SET content = replace(content,
  '- Porta occupata: run_command("lsof -t -i:PORTA | xargs kill -9") poi rilancia',
  '- Porta occupata: NON uccidere l''occupante (puo'' essere un servizio di un altro progetto; la rimozione forzata e'' dell''enforcer, non tua). Chiedi una porta con request_port e usa quella.')
WHERE key = 'system.nexus_base';

UPDATE nexus_prompt_templates SET content = replace(content,
  '  - "vedi se la porta e'' in ascolto" -> run_command con `ss -tlnp | grep :PORTA`.',
  '  - "vedi se la porta e'' in ascolto" -> run_command con l''attrezzo dell''host dichiarato in <ambiente_esecuzione> (Linux: `ss -tlnp | grep :PORTA`; Windows: `netstat -ano | findstr :PORTA`).')
WHERE key = 'system.nexus_base';

-- Guard: fuori da <privilegi_sistema> non devono restare prescrizioni di
-- attrezzi solo-Linux nude (ss/lsof senza l'alternativa dichiarata). Fallisce
-- la migrazione invece di lasciare il prompt che rimanda l'agente a tentativi.
DO $$
DECLARE
    corpo text;
    fuori_dal_blocco text;
BEGIN
    SELECT content INTO corpo FROM nexus_prompt_templates WHERE key = 'system.nexus_base';
    -- il blocco privilegi si toglie per confrontare solo il RESTO
    fuori_dal_blocco := regexp_replace(corpo, '<privilegi_sistema>.*</privilegi_sistema>', '', 's');
    IF fuori_dal_blocco ~ 'lsof' OR fuori_dal_blocco ~ '"ss -tlnp' OR fuori_dal_blocco ~ '`ss -tlnp[^`]*`\.' THEN
        RAISE EXCEPTION
            'system.nexus_base: resta una prescrizione solo-Linux nuda fuori da privilegi_sistema';
    END IF;
END $$;
