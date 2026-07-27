-- 0640: gate del dev-login, ora che il token lo firma il backend.
--
-- Perche' esiste. `GET /internal/settings/:key` e' montato FUORI dal layer di
-- autenticazione (crates/mcp-core/src/routes/public.rs) su un servizio in
-- ascolto su 0.0.0.0, e leggeva il valore RAW di qualunque chiave senza
-- guardare `is_secret`. Misurato sul cluster di sviluppo: rispondeva 200 e in
-- chiaro a `jwt_secret` (128 char), `anthropic_api_key`, `openai_api_key`,
-- `google_api_key`, `github_client_secret`. Con la chiave di firma si conia un
-- cookie di amministratore (la procedura era gia' scritta nel dev-login del
-- frontend). Il masking della LISTA guardava `is_secret`; la lettura puntuale no.
--
-- Il fix nel codice ha due parti: la rotta ora rifiuta le chiavi `is_secret`
-- (punto unico `nexus_auth::get_setting_public`), e il dev-login non legge piu'
-- il segreto — chiede al backend un token gia' firmato
-- (`POST /internal/dev-login-token`).
--
-- Questa chiave e' il gate di quell'endpoint: emette una credenziale di
-- amministratore, quindi deve poter essere spenta senza toccare il codice
-- (regola G: il gate vive nel DB, non in una `cfg!` di compilazione).
--
-- Seminata a 'true' perche' il dev-login era GIA' attivo su questi ambienti:
-- la migrazione non deve togliere a nessuno il modo di entrare. Su un'
-- installazione dove il dev-login non serve, si mette 'false' dal pannello
-- settings e l'endpoint risponde 403.
INSERT INTO settings (key, value, category, description, is_secret)
VALUES (
    'auth.dev_login_enabled',
    'true',
    'auth',
    'Abilita POST /internal/dev-login-token, che emette un JWT di amministratore per l''utente di sviluppo senza passare da OAuth. Endpoint di comodo per lo sviluppo locale: mettere a false dove non serve. Il token e'' firmato dal backend, la chiave di firma non lascia mai il processo.',
    FALSE
)
ON CONFLICT (key) DO NOTHING;
