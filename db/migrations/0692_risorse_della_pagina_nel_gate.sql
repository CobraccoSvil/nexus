-- 0692 — La resa guarda anche le RISORSE: un'immagine rotta e' un elemento reso.
--
-- ROOT CAUSE (misurata in esercizio il 09/08/2026). Il criterio `static_render`
-- (mig 0685) ha funzionato: ha bocciato un `index.html` valido da 4 KB che
-- rendeva ZERO elementi, l'agente ha corretto separando dati/rendering/avvio, e
-- alla rivalutazione la pagina e' passata. Il ciclo completo ha fatto il suo
-- lavoro. L'app approvata, pero', ha TUTTE le immagini rotte: le sei `<img>`
-- puntano a `https://via.placeholder.com/300x200?text=Prodotto+N`, un servizio
-- esterno oggi irraggiungibile, e nel progetto non esiste alcun file immagine.
--
-- PERCHE' I TRE SEGNALI DEL 0685 NON POTEVANO VEDERLO. Non e' una svista di
-- quel criterio, che fa esattamente cio' che dichiara: **un'immagine rotta E'
-- un elemento reso**. Il conteggio del DOM la conta, il contenitore dichiarato
-- ha i suoi sei figli, e nessuna eccezione e' stata lanciata. Tutti e tre i
-- segnali dicono il vero, e la pagina e' vuota all'occhio. Mancava la domanda:
-- «cio' che la pagina REFERENZIA e' arrivato?».
--
-- IL SEGNALE E' GIA' STRUTTURATO E GIA' RACCOLTO (regola M). Il browser
-- dichiara per ogni richiesta l'esito (`requestfailed` col proprio `errorText`,
-- o la risposta col proprio status) e il TIPO (`resourceType()`). Il tipo NON
-- si deduce dall'estensione dell'URL: `/api/thumb?id=3` e' un'immagine e
-- `/logo.png.txt` non lo e'. Nessuna seconda apertura della pagina: lo script
-- di `browser_probe` e' lo stesso dei criteri 0681 e 0685, e ha guadagnato due
-- campi (il tipo su ogni richiesta, l'URL finale della pagina), non un giro.
--
-- NON E' UN BOOLEANO (regola Q). «Tutte le immagini rotte» e «un'icona
-- decorativa mancante» sono fatti diversi. Il verdetto distingue: nessuna
-- fallita / alcune fallite / un TIPO compromesso / nessuna risorsa dichiarata /
-- non osservabile. Le ultime due sono l'ignoto DICHIARATO: una pagina
-- autosufficiente non e' rotta, e un'osservazione che non ha riportato le
-- richieste non assolve nessuno.
--
-- LA CAUSA E' SEPARATA DOVE E' STRUTTURALE. Una risorsa LOCALE che manca e'
-- sempre un errore dell'app (il file non esiste al percorso richiesto); una
-- ESTERNA che non risponde puo' essere la rete. Il discriminante e' l'ORIGINE
-- dell'URL confrontata con quella della pagina — un fatto, non un'euristica —
-- e dove una delle due non e' stabilibile la provenienza resta «indeterminata»,
-- mai indovinata. Le due cause NON danno verdetti diversi (a soglia raggiunta
-- la pagina e' rotta comunque, e chi la guarda non vede di chi sia la colpa):
-- danno RILIEVI diversi, perche' la correzione e' diversa.
--
-- BOCCIA O RIPORTA? Entrambe le cose, e il confine e' la soglia. E' la stessa
-- risoluzione della lente dello stile (mig 0655/0682): blocca SOLO il difetto
-- accertato, tutto il resto e' osservazione. Qui il difetto accertato e' il
-- TIPO COMPROMESSO — a soglia 1.0 significa «non una sola immagine di questa
-- pagina e' arrivata», che non e' opinabile e non e' spiegabile con una rete
-- lenta: e' la forma esatta del caso misurato (6 su 6). Le assenze sotto soglia
-- NON bocciano: entrano nell'evidenza del criterio anche quando passa, ed e'
-- deliberato — sono il solo dato con cui si potra' decidere, MISURANDO, se
-- 1.0 vada abbassato. La soglia parte dove non si puo' sbagliare e si stringe
-- sui numeri, non sull'intuito.
--
-- IL `font` RESTA FUORI dal vocabolario, per decisione e non per svista: un
-- carattere che non carica lascia il testo in un ripiego e la pagina mostra
-- comunque il proprio contenuto. E' la stessa ragione per cui il criterio 0685
-- non boccia sui `console.error`, e per cui il 0681 ha il vocabolario delle
-- terze parti: un rilievo su cio' che non si vede riporta i rimandi a vuoto.
--
-- Punto unico del criterio: nexus-agent-graph/src/decisions/risorse_pagina.rs
-- (puro). Lo consuma `static_render::classifica_resa` come QUARTA causa;
-- confine col browser: mcp-core/src/agent_tools/browser_probe.rs.
--
-- ROLLBACK senza codice ne' redeploy: svuotare i tipi governati
--   UPDATE settings SET value='' WHERE key='agent.final_gate.static_render_resource_types';
-- Il criterio dichiara `not_observable` sulle risorse e la resa torna ai tre
-- segnali del 0685. Se invece a essere rumoroso e' un tipo solo, il rimedio e'
-- toglierlo dall'elenco, non spegnere il criterio.

INSERT INTO settings (key, value, category, description, is_secret)
VALUES
  (
    'agent.final_gate.static_render_resource_types',
    'image,stylesheet,script,media',
    'agent',
    'Tipi di risorsa (vocabolario `resourceType()` del browser) la cui assenza si vede guardando la pagina, e che il criterio della resa percio'' governa. Il `font` ne resta fuori: un carattere che non carica lascia il testo in un ripiego e la pagina mostra comunque il proprio contenuto. Elenco vuoto = il criterio dichiara di non rispondere sulle risorse (nessun ripiego nel codice, regola G).',
    false
  ),
  (
    'agent.final_gate.static_render_broken_resource_ratio',
    '1.0',
    'agent',
    'Quota di risorse fallite, RAPPORTATA AL TIPO, da cui quel tipo si dice compromesso e la pagina e'' bocciata. 1.0 = nessuna risorsa di quel tipo e'' arrivata, il caso non opinabile (misurato: 6 immagini su 6 verso un dominio irraggiungibile). Le assenze sotto soglia si riportano nell''evidenza e non bocciano: sono il dato con cui abbassare questa soglia misurando, invece che a intuito. Chiave assente = criterio muto sulle risorse.',
    false
  )
ON CONFLICT (key) DO NOTHING;

-- Guard: le due chiavi devono esistere. Senza, il criterio delle risorse e'
-- muto (`not_observable`) e la migrazione sarebbe passata a vuoto — cioe' il
-- buco misurato il 09/08 resterebbe aperto senza che nulla lo dichiari.
DO $$
DECLARE
  presenti INT;
BEGIN
  SELECT COUNT(*) INTO presenti
  FROM settings
  WHERE key IN (
    'agent.final_gate.static_render_resource_types',
    'agent.final_gate.static_render_broken_resource_ratio'
  );
  IF presenti <> 2 THEN
    RAISE EXCEPTION 'mig 0692: attese 2 chiavi delle risorse di pagina, trovate %', presenti;
  END IF;
END $$;

-- Guard: la soglia deve essere un numero interpretabile. Un valore che il
-- codice non sa leggere non produce un errore visibile: degrada a "criterio
-- muto", cioe' allo stesso silenzio di prima, e nessuno se ne accorgerebbe.
DO $$
DECLARE
  grezzo TEXT;
BEGIN
  SELECT value INTO grezzo FROM settings
  WHERE key = 'agent.final_gate.static_render_broken_resource_ratio';
  IF grezzo !~ '^[0-9]+(\.[0-9]+)?$' THEN
    RAISE EXCEPTION
      'mig 0692: soglia risorse non numerica (%): il criterio resterebbe muto in silenzio', grezzo;
  END IF;
END $$;

-- Guard: questo criterio VIVE dentro quello della resa statica (mig 0685). Se
-- quella chiave non esistesse, le due righe qui sopra sarebbero configurazione
-- di un criterio che non nasce mai.
DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM settings WHERE key = 'agent.final_gate.static_render_enabled'
  ) THEN
    RAISE EXCEPTION
      'mig 0692: manca agent.final_gate.static_render_enabled (mig 0685), criterio ospite di queste chiavi';
  END IF;
END $$;
