import { NextRequest, NextResponse } from 'next/server';
import { proxyRequest } from '../../../_proxy';

/**
 * POST /api/admin/prompt-templates/:key/preview
 *
 * Proxy verso admin-service che ritorna il prompt resolved con i placeholder
 * sostituiti (lang_hint, type_hint, repo_summary). Body:
 *   { intent?: string, repo_lang?: string, repo_summary?: string }
 */
export async function POST(
  req: NextRequest,
  { params }: { params: Promise<Record<string, string>> },
): Promise<NextResponse> {
  const p = await params;
  const path = `/api/admin/prompt-templates/:key/preview`.replace(
    /:([\w]+)/g,
    (_, k) => p[k] ?? `:${k}`,
  );
  return proxyRequest(req, path, 'POST');
}
