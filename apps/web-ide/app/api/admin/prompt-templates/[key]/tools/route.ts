import { NextRequest, NextResponse } from 'next/server';
import { proxyRequest } from '../../../_proxy';

export async function GET(req: NextRequest, { params }: { params: Promise<Record<string,string>> }): Promise<NextResponse> {
  const p = await params;
  const path = `/api/admin/prompt-templates/:key/tools`.replace(/:([\w]+)/g, (_, k) => p[k] ?? `:${k}`);
  return proxyRequest(req, path, 'GET');
}

export async function PUT(req: NextRequest, { params }: { params: Promise<Record<string,string>> }): Promise<NextResponse> {
  const p = await params;
  const path = `/api/admin/prompt-templates/:key/tools`.replace(/:([\w]+)/g, (_, k) => p[k] ?? `:${k}`);
  return proxyRequest(req, path, 'PUT');
}
