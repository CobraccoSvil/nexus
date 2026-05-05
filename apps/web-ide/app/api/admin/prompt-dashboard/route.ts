import { NextRequest } from 'next/server';
import { proxyToAdmin } from '../browser-bridge/_proxy_bb';

export async function GET(req: NextRequest) {
  return proxyToAdmin(req, '/api/admin/prompt-dashboard', 'GET');
}
