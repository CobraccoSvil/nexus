import { NextRequest, NextResponse } from 'next/server';
import { proxyRequest } from '../_proxy';

export async function POST(req: NextRequest): Promise<NextResponse> {
  return proxyRequest(req, '/api/admin/sync-model-catalog', 'POST');
}
