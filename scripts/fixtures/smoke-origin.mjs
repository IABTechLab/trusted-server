#!/usr/bin/env node
// Owned loopback origin for real SDK upload and duplicate-header smoke checks.
import assert from 'node:assert/strict'
import http from 'node:http'

const port = Number(process.argv[2])
assert(Number.isInteger(port) && port > 0 && port <= 65535)
const server = http.createServer(async (request, response) => {
  if (request.method === 'POST' && request.url === '/native-upload') {
    const chunks = []
    let size = 0
    for await (const chunk of request) {
      size += chunk.length
      if (size > 65536) {
        response.writeHead(413).end()
        return
      }
      chunks.push(chunk)
    }
    const body = JSON.stringify({
      received: Buffer.concat(chunks).toString('utf8'),
    })
    response
      .writeHead(200, {
        'content-type': 'application/json',
        'content-length': Buffer.byteLength(body),
        'set-cookie': ['fixture_a=one; Path=/', 'fixture_b=two; Path=/'],
      })
      .end(body)
    return
  }
  const body = `<!doctype html><html><body>SMOKE_ORIGIN_SENTINEL<a href="http://127.0.0.1:${port}/asset">asset</a></body></html>`
  response
    .writeHead(200, {
      'content-type': 'text/html',
      'content-length': Buffer.byteLength(body),
    })
    .end(body)
})
server.listen(port, '127.0.0.1')
for (const signal of ['SIGINT', 'SIGTERM']) {
  process.once(signal, () => {
    server.closeIdleConnections()
    server.close(() => process.exit(0))
    setTimeout(() => process.exit(1), 2000).unref()
  })
}
