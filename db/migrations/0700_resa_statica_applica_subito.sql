-- 0700 — La resa di un'app senza server si APPLICA, non si osserva.
--
-- Corregge la sola decisione sbagliata della 0699, che resta applicata e non si
-- tocca: quel file ha gia' un checksum registrato, e riscriverlo renderebbe il
-- DB non migrabile (incidente gia' avuto con le migrazioni 0117/0118). Il
-- razionale che segue SOSTITUISCE il paragrafo «PERCHE' observe E NON enforce»
-- della 0699.
--
-- COSA CAMBIA: `agent.final_gate.static_render_mode` passa da `observe` a
-- `enforce`. Il vocabolario resta a tre valori e la modalita' resta
-- configurabile — serve da kill-switch e da strumento di taratura — ma il
-- valore con cui il sistema gira e' `enforce`, per TUTTE le pagine: sia quelle
-- che il run ha scritto sia quelle rilevate sull'albero. Nessuna distinzione per
-- provenienza; la provenienza resta nell'evidenza, che e' il posto in cui serve
-- (chi legge un rosso deve sapere se il gate ha guardato il lavoro di questo run
-- o cio' che ha trovato).
--
-- PERCHE' SI PARTE APPLICANDO, e non in osservazione.
--
--   1. `observe` NON CHIUDE IL CASO CHE MOTIVA IL FIX. La 0699 nasce da
--      `test-11-08-listino`: pagina `listino.html` rotta (Uncaught SyntaxError,
--      contenitore `productsGrid` a 0 figli, body di 90 caratteri) e run chiuso
--      «task complete». Con la risoluzione tardiva della pagina il criterio ora
--      NASCE e la MISURA e' negativa — ma in osservazione l'esito resta
--      `Passed`, quindi quel run si chiuderebbe di nuovo «completato». Si
--      sarebbe pagato l'intero lavoro per continuare a non fermare l'unico caso
--      per cui e' stato fatto.
--
--   2. IL CRITERIO NON E' NUOVO: e' in esercizio dalla 0685, ed era ACCESO. La
--      chiave `agent.final_gate.static_render_enabled` valeva `true` sul DB di
--      produzione, con la conseguenza piena (un verdetto negativo bocciava).
--      Mapparla su `observe` non sarebbe stato «un criterio nuovo che entra con
--      prudenza»: sarebbe stato un DEPOTENZIAMENTO di una difesa gia' in
--      esercizio, e i progetti che oggi vengono correttamente bocciati
--      smetterebbero di esserlo.
--
--   3. LA MISURA E' LA STESSA NEI DUE REGIMI. La modalita' non entra nel merito
--      del verdetto (lo compone `esito_resa`, la conseguenza la applica
--      `in_osservazione` dopo): non c'e' un «prima misuriamo meglio» da
--      guadagnare aspettando. Cio' che cambia e' solo se il rosso ferma il run.
--
-- IL RISCHIO NOTO, dichiarato e non nascosto: la POPOLAZIONE NUOVA. Risolvere la
-- pagina alla verifica consegna al criterio dei run che prima non lo vedevano
-- mai — quelli che generano il sito da zero, dove a t=0 non c'era pagina da
-- rilevare. Il caso prevedibile e' una SPA scaffoldata DURANTE il run: il suo
-- `index.html` referenzia un modulo (`/src/main.tsx`) che la route di anteprima
-- serve con un content-type generico (`application/octet-stream`), il bundle non
-- parte e la pagina resta vuota per costruzione — non per colpa dell'agente. Se
-- i primi run mostrassero falsi rossi di questa forma, il RIPIEGO e' `observe`
-- (una riga di UPDATE, nessun deploy), e la correzione vera sta nel
-- content-type della route di anteprima, non nella soglia del criterio.
--
-- Quello che NON si fa e' fermare tutti gli altri run per un difetto ipotizzato:
-- il caso misurato e' reale e documentato, questo e' previsto. Fra una difesa
-- che agisce e una che guarda, con un incidente in mano e uno in ipotesi, si
-- sceglie quella che agisce, e si tiene pronto il ripiego.
--
-- RIPIEGO (falsi rossi nei primi run):
--   UPDATE settings SET value='observe'
--    WHERE key='agent.final_gate.static_render_mode';
-- SPEGNIMENTO COMPLETO (kill-switch):
--   UPDATE settings SET value='off'
--    WHERE key='agent.final_gate.static_render_mode';

UPDATE settings
   SET value = 'enforce',
       description =
         'Quanto pesa sul run il criterio «l''app senza server mostra il proprio contenuto?». '
         '`off` = il criterio non nasce; `observe` = apre la pagina, misura e SCRIVE l''evidenza '
         '(cause comprese) senza mai bocciare; `enforce` = un verdetto negativo boccia il run. '
         'La pagina misurata e'' quella che il run ha SCRITTO (registro file_mutations), risolta al '
         'momento della verifica e non alla costruzione del motore; dove il run non ha scritto '
         'pagine si ripiega sul rilevamento dell''albero, dichiarandolo nell''evidenza. '
         'Default `enforce`, per ogni pagina e senza distinzione di provenienza: il criterio e'' in '
         'esercizio dalla mig 0685 con la conseguenza piena, e osservare e basta lascerebbe passare '
         'proprio il caso per cui la risoluzione tardiva e'' stata fatta (pagina rotta, run chiuso '
         '«task complete»). `observe` resta disponibile come ripiego se emergessero falsi rossi.',
       updated_at = NOW()
 WHERE key = 'agent.final_gate.static_render_mode';

-- Guard: la chiave deve esistere (la crea la 0699) ed essere finita su
-- `enforce`. Un UPDATE che non tocca alcuna riga passa in silenzio, e il
-- criterio resterebbe a osservare senza che nulla lo dichiari.
DO $$
DECLARE
  modalita TEXT;
BEGIN
  SELECT value INTO modalita FROM settings
   WHERE key = 'agent.final_gate.static_render_mode';
  IF modalita IS NULL THEN
    RAISE EXCEPTION
      'mig 0700: manca agent.final_gate.static_render_mode (la crea la 0699)';
  END IF;
  IF modalita <> 'enforce' THEN
    RAISE EXCEPTION
      'mig 0700: modalita'' attesa enforce, trovata %', modalita;
  END IF;
END $$;
