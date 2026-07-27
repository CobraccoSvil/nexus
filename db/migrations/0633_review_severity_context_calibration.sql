-- 0633_review_severity_context_calibration.sql
--
-- CAUSA (diagnosi 21/07, incidente app-todo-b1 failed_diagnosed): la Review
-- adversariale del ReviewGate bocciava app FUNZIONANTI per problemi di hardening
-- di PRODUZIONE assenti in un PROTOTIPO locale (CORS '*', URL localhost hardcoded),
-- marcati severity 'alta'. severity.rs::any_high + orchestrator.review_fail_on_high
-- _severity=true -> un solo finding 'alta' abilita il veto avversario in minoranza
-- -> rimando (max 3 cicli) -> RejectedFinal. I prompt del revisore
-- (subagent.review.base, system.reviewer) istruivano ad assegnare la severity in
-- ASSOLUTO, senza nozione del contesto d'uso: un security-audit assoluto trova
-- sempre CORS '*' 'alto', anche per un'app che gira solo su localhost.
--
-- FIX (calibrazione, regola H alla RADICE - il prompt, non il veto): aggiunge ai
-- prompt del revisore una direttiva di CONTESTO. La gravita' si calibra sul target
-- reale del progetto; il default dei progetti Nexus e' prototipo in sviluppo locale
-- (salvo segnali espliciti di produzione). 'alta' (che e' veto bloccante) resta per
-- bug FUNZIONALI che rompono il task o vulnerabilita' SFRUTTABILI nel target reale;
-- NON per hardening di produzione mancante in un prototipo (CORS permissivo, URL/
-- porte localhost, assenza di HTTPS/rate-limiting/security header, credenziali di
-- sviluppo), se il task non lo chiedeva. I veti su difetti REALI restano intatti:
-- NON si tocca orchestrator.review_fail_on_high_severity (disattivarlo perderebbe i
-- veti legittimi - scartato).
--
-- Reversibile a caldo (regola G): i template hanno cache <=60s; nessun redeploy
-- necessario oltre a quello che applica questa migrazione. Idempotente (append solo
-- se la sezione non e' gia' presente).

UPDATE nexus_prompt_templates
   SET content = content || $md$

<calibrazione_gravita>
CONTESTO D'USO (calibra la severity su questo): i progetti costruiti qui sono per
default PROTOTIPI in SVILUPPO LOCALE (girano su localhost, non ancora in produzione),
salvo segnali espliciti del contrario (Dockerfile/compose di produzione, dominio
pubblico, config di deploy, README che dichiara la produzione).

La severity determina il VETO: un solo finding 'alta' fa FALLIRE il run. Quindi
riserva 'alta' a difetti che rompono l'obiettivo NEL CONTESTO REALE del progetto:
- bug FUNZIONALI che impediscono al task richiesto di funzionare (endpoint rotto,
  crash, dato perso, feature assente rispetto alla richiesta);
- vulnerabilita' SFRUTTABILI nel target reale (injection su input utente, secret
  reali committati, autenticazione aggirabile su un servizio davvero esposto).

NON marcare 'alta' (al massimo 'media'/'bassa' o suggerimento) l'hardening di
PRODUZIONE assente in un prototipo locale, se il task non lo chiedeva:
- CORS permissivo / Access-Control-Allow-Origin '*';
- URL/host 'localhost' o porte hardcoded, valori di config di sviluppo in chiaro;
- assenza di rate limiting, HTTPS, security header, CSRF token;
- credenziali/chiavi placeholder di sviluppo.

Se il task dell'utente chiedeva ESPLICITAMENTE robustezza di produzione, questi
tornano rilevanti e la gravita' sale. In dubbio sul contesto deducilo dai file
(localhost, .env di dev, assenza di deploy) e dichiara l'assunzione nel finding.
</calibrazione_gravita>$md$
 WHERE key = 'subagent.review.base'
   AND content NOT LIKE '%calibrazione_gravita%';

UPDATE nexus_prompt_templates
   SET content = content || $md$

Calibrazione della gravita' sul CONTESTO: i progetti qui sono per default prototipi
in sviluppo locale (localhost), non in produzione, salvo segnali espliciti. Un solo
finding di gravita' alta/bloccante fa fallire il run: riservala a bug FUNZIONALI che
rompono il task o a vulnerabilita' SFRUTTABILI nel target reale. NON trattare come
bloccante l'hardening di produzione mancante in un prototipo locale (CORS permissivo,
URL/porte localhost hardcoded, assenza di HTTPS/rate-limiting/security header,
credenziali di sviluppo) se il task non lo richiedeva: segnalalo al piu' come
suggerimento. Se il task chiedeva robustezza di produzione, la gravita' sale.$md$
 WHERE key = 'system.reviewer'
   AND content NOT LIKE '%Calibrazione della gravita%';
