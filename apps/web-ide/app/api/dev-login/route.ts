import { NextResponse } from 'next/server';
import crypto from 'crypto';

// Route SOLO per sviluppo locale — imposta il cookie JWT senza OAuth.
// NOTA: il vecchio dev_login_server.py (localhost:9999) e' stato RIMOSSO
// (migrazione zero-Python) e con esso l'inserimento della riga in `sessions`:
// il dev-login si regge sul solo cookie JWT, che e' il percorso gia' in uso.
// Se servisse di nuovo la riga esplicita in Postgres, va reimplementata lato
// Node/Rust (mai un nuovo server Python).
export async function GET(request: Request) {
  if (process.env.NODE_ENV === 'production') {
    return NextResponse.json({ error: 'Not available in production' }, { status: 403 });
  }

  try {
    const reqUrl = new URL(request.url);

    // Leggi jwt_secret direttamente da nexus-core (bypass gateway).
    // 127.0.0.1 e non localhost: su Windows la risoluzione prova prima ::1 e
    // paga ~2s di timeout per richiesta quando il core ascolta su IPv4.
    const coreUrl = process.env.CORE_SERVICE_URL || 'http://127.0.0.1:4000';
    const secretRes = await fetch(`${coreUrl}/internal/settings/jwt_secret`, { cache: 'no-store' });
    if (!secretRes.ok) {
      return NextResponse.json({ error: `Cannot read jwt_secret from core: ${secretRes.status}` }, { status: 500 });
    }
    const body = await secretRes.json() as { value?: string };
    const jwtSecret = body.value;
    if (!jwtSecret) {
      return NextResponse.json({ error: 'jwt_secret empty' }, { status: 500 });
    }

    const userId = 'e9a5dc7e-7936-4f7b-9ff3-11bb175041d8';
    const role = 'admin';

    // Genera JWT HS256 con solo Node.js crypto (no dipendenze extra)
    const exp = Math.floor(Date.now() / 1000) + 86400 * 7;
    const header  = Buffer.from(JSON.stringify({ alg: 'HS256', typ: 'JWT' })).toString('base64url');
    const payload = Buffer.from(JSON.stringify({ sub: userId, role, exp })).toString('base64url');
    const sig     = crypto.createHmac('sha256', jwtSecret).update(`${header}.${payload}`).digest('base64url');
    const token   = `${header}.${payload}.${sig}`;

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
