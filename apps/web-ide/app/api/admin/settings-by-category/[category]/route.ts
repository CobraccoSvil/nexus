import { NextRequest, NextResponse } from 'next/server';
import { proxyRequest } from '../../_proxy';

export async function GET(req: NextRequest, { params }: { params: Promise<Record<string,string>> }): Promise<NextResponse> {
  const p = await params;
  const path = `/api/admin/settings-by-category/:category`.replace(/:([\w]+)/g, (_, k) => p[k] ?? `:${k}`);
  return proxyRequest(req, path, 'GET');
}
