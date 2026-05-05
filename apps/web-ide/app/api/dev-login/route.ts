import { NextResponse } from 'next/server';
import crypto from 'crypto';

// Route SOLO per sviluppo locale — imposta il cookie JWT senza OAuth
// La sessione viene inserita dal dev_login_server.py (localhost:9999)
export async function GET(request: Request) {
  if (process.env.NODE_ENV === 'production') {
    return NextResponse.json({ error: 'Not available in production' }, { status: 403 });
  }

  try {
    const reqUrl = new URL(request.url);
    const _origin = reqUrl.origin;

    // Leggi jwt_secret direttamente da nexus-core (bypass gateway, porta 4000)
    const coreUrl = 'http://localhost:4000';
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
    const tokenHash = crypto.createHash('sha256').update(token).digest('hex');

    // Inserisci sessione tramite Python dev-server (ha accesso diretto a Postgres)
    try {
      await fetch(
        `http://localhost:9999/insert-session?user_id=${encodeURIComponent(userId)}&hash=${encodeURIComponent(tokenHash)}`,
        { signal: AbortSignal.timeout(3000) }
      );
    } catch {
      // non critico se fallisce — potrebbe esistere già
    }

    // Redirect a /ide — hardcoded su localhost:3000 per dev
    const response = NextResponse.redirect('http://localhost:3000/ide');
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
