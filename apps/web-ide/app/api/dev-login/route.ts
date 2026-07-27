import { NextResponse } from 'next/server';

// Route SOLO per sviluppo locale — imposta il cookie JWT senza OAuth.
// NOTA: il vecchio dev_login_server.py (localhost:9999) e' stato RIMOSSO
// (migrazione zero-Python) e con esso l'inserimento della riga in `sessions`:
// il dev-login si regge sul solo cookie JWT, che e' il percorso gia' in uso.
// Se servisse di nuovo la riga esplicita in Postgres, va reimplementata lato
// Node/Rust (mai un nuovo server Python).
//
// Il token lo FIRMA il backend. Prima questa route scaricava `jwt_secret` da
// `GET /internal/settings/jwt_secret` e coniava il JWT qui: quella rotta e'
// senza autenticazione su un servizio in ascolto su 0.0.0.0, quindi la chiave
// di firma della piattaforma era leggibile da chiunque raggiungesse la porta —
// e con quella si conia un token di amministratore. Ora il segreto non lascia
// il backend: qui arriva solo il token gia' firmato.
export async function GET(request: Request) {
  if (process.env.NODE_ENV === 'production') {
    return NextResponse.json({ error: 'Not available in production' }, { status: 403 });
  }

  try {
    const reqUrl = new URL(request.url);

    // 127.0.0.1 e non localhost: su Windows la risoluzione prova prima ::1 e
    // paga ~2s di timeout per richiesta quando il core ascolta su IPv4.
    const coreUrl = process.env.CORE_SERVICE_URL || 'http://127.0.0.1:4000';
    const tokenRes = await fetch(`${coreUrl}/internal/dev-login-token`, {
      method: 'POST',
      cache: 'no-store',
    });
    if (!tokenRes.ok) {
      const detail = await tokenRes.text().catch(() => '');
      return NextResponse.json(
        { error: `dev-login token non emesso dal core: ${tokenRes.status} ${detail}` },
        { status: tokenRes.status === 403 ? 403 : 500 },
      );
    }
    const body = (await tokenRes.json()) as { token?: string };
    const token = body.token;
    if (!token) {
      return NextResponse.json({ error: 'token assente nella risposta del core' }, { status: 500 });
    }

    // Redirect a /ide SULLA STESSA origine della richiesta: l'indirizzo era
    // fissato su localhost:3000, quindi con il web-ide su un'altra porta il
    // login rimandava a un'altra istanza (o a un orfano rimasto su :3000) e il
    // cookie appena impostato sembrava non funzionare.
    const response = NextResponse.redirect(new URL('/ide', reqUrl.origin));
    response.cookies.set('token', token, {
      httpOnly: false,  // dev only
      secure: false,
      sameSite: 'lax',
      maxAge: 604800,
      path: '/',
    });
    return response;
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : String(err);
    return NextResponse.json({ error: msg }, { status: 500 });
  }
}
