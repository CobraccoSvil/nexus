import { NextRequest, NextResponse } from 'next/server';
import { proxyRequest } from '../../_proxy';

export async function GET(req: NextRequest): Promise<NextResponse> {
  return proxyRequest(req, '/api/admin/gateway/providers', 'GET');
}
