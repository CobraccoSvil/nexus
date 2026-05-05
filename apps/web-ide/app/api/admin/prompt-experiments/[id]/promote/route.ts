import { NextRequest } from 'next/server';
import { proxyRequest } from '../../../_proxy';

export async function POST(req: NextRequest, { params }: { params: Promise<{ id: string }> }) {
  const p = await params;
  const path = `/api/admin/prompt-experiments/:id/promote`.replace(
    /:([\w]+)/g, (_, k) => (p as Record<string, string>)[k] ?? `:${k}`
  );
  return proxyRequest(req, path, 'POST');
}
