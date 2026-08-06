-- 0682 — Lo stile dichiarato dal codice entra nel final gate.
--
-- ROOT CAUSE (misurata il 06/08/2026 su agenda-medica). La lente che risponde
-- alla domanda «le classi che il codice scrive hanno una fonte che le produce?»
-- esisteva gia', completa, con i suoi test e il suo vocabolario nel DB
-- (`ui_styling`, mig 0655) — ed era senza effetto sulla chiusura di un run:
-- offerta come tool a due figure, e nessun nodo del grafo la interrogava.
--
-- Il difetto non e' che la misura mancasse: e' che nessuno la CONSUMAVA. Nel run
-- misurato il tool `ui_styling_audit` era perfino stato CHIAMATO dall'agente.
-- Le prove raccolte a mano sull'app consegnata:
--   classi scritte nei componenti  -> si' (`min-h-screen bg-gray-50`,
--                                    `max-w-7xl mx-auto`, `flex justify-between`)
--   `tailwindcss` in package.json  -> si' (e fa da ALIBI a chi controlla)
--   tailwind.config / postcss.config -> NESSUNO
--   file .css nel progetto         -> NESSUNO
--   import di un foglio nei sorgenti -> NESSUNO
-- Cioe' `FrameworkNonConfigurato`, che la lente sa gia' distinguere da
-- `NessunaFonte`. La pagina servita era HTML grezzo — verificato caricandola in
-- un browser reale — e il run si e' chiuso «completato».
--
-- E' la stessa lezione della mig 0681, applicata alla misura che quella
-- migrazione citava come esempio del difetto: una lente che nessun gate
-- interroga si e' costruita, non e' entrata in esercizio.
--
-- PERCHE' DETERMINISTICO E NON UN GIUDICE. «Bello» non e' un criterio, e un
-- giudice di gusto senza metro moltiplica i rimandi a vuoto (misurati: un run
-- del 27/07, 3 rimandi, 2,1M token, 3,08 USD). Questo non e' gusto: o esiste
-- qualcosa che rende quelle classi, o non esiste. Nessuna chiamata al modello.
--
-- COSA NON BOCCIA. Solo `stile_dichiarato_non_applicato` fallisce. Un progetto
-- senza interfaccia, o che non dichiara classi affatto, PASSA — codice grezzo e
-- onesto non e' un difetto. E cio' che la lente non ha potuto accertare
-- (vocabolario assente, fonti fuori dalla radice esaminata) resta INCONCLUDENTE
-- col motivo dichiarato: il run chiude `completed_unverified`, mai bocciato su
-- un non-verdetto. Sono le distinzioni che la lente fa apposta, e il gate le
-- rispetta invece di appiattirle.
--
-- Punto unico del criterio: crates/nexus-agent-tools/src/ui_styling.rs
-- (`classify_styling`, puro; `CRITERION_TYPE` accanto ad esso). Il criterio del
-- gate lo costruisce `mcp-core::native_engine::criterio_stile` — non il nodo,
-- che non vede quel crate — e l'unico I/O sta in `criteria_runner`.
--
-- ROLLBACK: UPDATE settings SET value = 'false'
--            WHERE key = 'agent.final_gate.ui_styling_enabled';

INSERT INTO settings (key, value, description, category)
VALUES (
    'agent.final_gate.ui_styling_enabled',
    'true',
    'Final gate: il codice dichiara classi di stile che nessuna fonte applica? '
    'Deterministico (nessun modello), delega la lente ui_styling. Boccia SOLO il '
    'difetto conclamato; cio'' che non si e'' potuto accertare resta inconcludente.',
    'agent'
)
ON CONFLICT (key) DO UPDATE
    SET value = EXCLUDED.value,
        description = EXCLUDED.description;
