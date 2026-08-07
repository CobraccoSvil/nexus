-- 0684 — Un `rm -rf` sulla cache di build non e' irreversibile.
--
-- ROOT CAUSE (misurata il 07/08/2026 su gestione-corsi). Le regole di
-- `orchestrator.critical_step_rules` classificano guardando il VERBO: il token
-- `rm -rf` vale `irreversible` qualunque cosa segua. E' giusto come INNESCO —
-- il verdetto sul passo resta agentico — ma «irreversibile» e' un'affermazione
-- sull'OGGETTO: dice che cio' che sparisce non torna.
--
-- Su `node_modules`, `.next`, `dist`, `target` e' falso per costruzione: quei
-- percorsi sono l'OUTPUT di un comando che il progetto sa rieseguire.
-- Cancellarli e' il gesto piu' ordinario di un ciclo di sviluppo.
--
-- COSA E' ACCADUTO. Il passo `cd school-courses-fe && rm -rf .next
-- node_modules/.cache` e' stato classificato irreversibile. Il gate duale
-- pretende due giudici su fornitori distinti dall'esecutore (mistral) e non li
-- ha trovati: anthropic, openai e perplexity erano in cooldown di credito, e i
-- cooldown brevi per rate limit — che fino al commit 09a9195d escludevano
-- l'intero fornitore invece del solo modello — svuotavano il resto. Con zero
-- verdetti il gate e' fail-closed, e in modalita' autonoma non c'e' nessuno a
-- cui chiedere: il passo e' rimasto non eseguito e l'agente lo ha riproposto al
-- giro dopo (iterazioni 22001 e 23001 dello stesso run).
--
-- Un gate di sicurezza che ferma la pulizia della cache non protegge nulla, e
-- ferma tutto.
--
-- IL FIX (`step_gate::declassa_se_rigenerabile`): un `Irreversible` che colpisce
-- SOLO artefatti rigenerabili scende a `Critical`. Resta sorvegliato — il gate
-- lo valuta ancora — ma non e' piu' fail-closed sull'assenza di giudici.
--
-- I due criteri sono entrambi necessari, e il secondo e' quello che tiene:
--   1. ogni bersaglio e' in questo vocabolario;
--   2. ogni bersaglio e' RELATIVO e non risale (`..`, path assoluti, unita' di
--      Windows sono esclusi) — fuori dal progetto il nome di una cartella non
--      dice piu' di chi sia.
-- Un solo bersaglio che non li soddisfa tiene irreversibile l'intero comando:
-- `rm -rf .next src` cancella anche i sorgenti, e la presenza di un artefatto
-- rigenerabile sulla stessa riga non lo rende meno definitivo.
--
-- PERCHE' NEL DB E NON NEL CODICE (regola G). Il nome della cartella di build
-- cambia col framework — `.next`, `.nuxt`, `.svelte-kit`, `.angular` sono
-- arrivati uno dopo l'altro — e inseguirli a codice e' la toppa che la regola H
-- vieta. Qui si aggiunge una voce e il sistema la sa al refresh successivo.
--
-- VUOTO = NESSUN DECLASSAMENTO, cioe' il comportamento precedente. Svuotare
-- questa chiave e' il rollback.

INSERT INTO settings (key, value, category, description, is_secret)
VALUES (
    'orchestrator.rebuildable_artifacts',
    'node_modules,.next,.nuxt,.svelte-kit,.angular,.turbo,.parcel-cache,.cache,dist,build,out,target,__pycache__,.pytest_cache,.mypy_cache,.ruff_cache,coverage,.gradle,bin,obj',
    'orchestrator',
    'Cartelle che un progetto sa RIGENERARE con un comando di build/install. Un `rm -rf` che colpisce solo queste, con percorsi relativi che non risalgono, scende da irreversibile a critico nel gate duale sui passi (mig 0684). Vuoto = nessun declassamento.',
    FALSE
)
ON CONFLICT (key) DO NOTHING;
