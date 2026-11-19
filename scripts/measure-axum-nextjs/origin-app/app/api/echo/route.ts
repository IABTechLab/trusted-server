import { createHash } from 'node:crypto';
async function echo(request: Request) {
  const body = Buffer.from(await request.arrayBuffer());
  return Response.json({
    method: request.method,
    bytes: body.length,
    sha256: createHash('sha256').update(body).digest('hex'),
    cookie: request.headers.get('cookie'),
    contentType: request.headers.get('content-type'),
  });
}
export const POST = echo;
export const GET = echo;
