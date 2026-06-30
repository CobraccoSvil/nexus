import { NextRequest, NextResponse } from 'next/server';
import { proxyRequest } from '../../../../_proxy';

export async function DELETE(req: NextRequest, { params }: { params: Promise<Record<string,string>> }): Promise<NextResponse> {
  const p = await params;
  const path = `/api/admin/projects/:projectId/members/:userId`.replace(/:([\w]+)/g, (_, k) => p[k] ?? `:${k}`);
  return proxyRequest(req, path, 'DELETE');
}
