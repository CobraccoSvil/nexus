import { NextRequest, NextResponse } from 'next/server'
import fs from 'fs'
import path from 'path'

export async function POST(req: NextRequest) {
  const { name, data } = await req.json()
  if (!name || !data) return NextResponse.json({ error: 'missing params' }, { status: 400 })

  const safeName = path.basename(name).replace(/[^a-z0-9._-]/gi, '_')
  const destDir = path.join(process.cwd(), 'public', 'screenshots')
  fs.mkdirSync(destDir, { recursive: true })
  const destPath = path.join(destDir, safeName)

  const buf = Buffer.from(data, 'base64')
  fs.writeFileSync(destPath, buf)
  return NextResponse.json({ ok: true, path: `/screenshots/${safeName}`, bytes: buf.length })
}
