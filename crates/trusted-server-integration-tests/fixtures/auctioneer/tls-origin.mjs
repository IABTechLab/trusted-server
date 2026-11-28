// Controlled demand and asset listeners. Rust owns this child and all ports.
import http from 'node:http'
import https from 'node:https'
import { appendFileSync, readFileSync, writeFileSync } from 'node:fs'
import { createRequire } from 'node:module'
const require = createRequire(import.meta.url)
const { pucPage } = require('../../browser/initial-render/pages.cjs')
const settings = JSON.parse(readFileSync('/work/listeners.json', 'utf8'))
const prebid = JSON.parse(readFileSync('/inputs/prebid/manifest.json', 'utf8'))
const bundle = readFileSync(`/inputs/prebid/${prebid.filename}`)
const puc = readFileSync(
  new URL(
    '../../browser/node_modules/prebid-universal-creative/dist/banner.js',
    import.meta.url
  )
)
const servers = []
let sequence = 0
function record(event) {
  appendFileSync('/results/receipts.jsonl', JSON.stringify(event) + '\n')
}
function page(empty) {
  return `<!doctype html><html><head><title>Production PBS baseline</title></head><body><div id="${empty ? 'empty-slot' : 'example-slot'}" style="width:300px;height:250px"></div></body></html>`
}
async function handle(request, response) {
  const url = new URL(request.url, 'http://localhost')
  const body = []
  for await (const part of request) {
    body.push(part)
    if (Buffer.concat(body).length > 1024 * 1024) {
      response.writeHead(413).end()
      return
    }
  }
  const data = Buffer.concat(body).toString('utf8')
  record({
    method: request.method,
    path: request.url,
    host: request.headers.host,
    sni: request.socket.servername ?? null,
    body: data,
    port: request.socket.localPort,
  })
  response.setHeader('cache-control', 'no-store')
  if (url.pathname === '/health') return response.end('ready')
  if (url.pathname === '/redirect-off-list') {
    response
      .writeHead(302, { location: 'https://off-list.example.com/pixel' })
      .end()
    return
  }
  if (url.pathname === '/openrtb2/auction') {
    const auction = JSON.parse(data)
    const mode = JSON.parse(readFileSync('/work/bid-mode.json', 'utf8'))
    if (mode.failure) {
      record({ fixtureResponse: 'failure', auctionId: auction.id, status: 500 })
      response
        .writeHead(500, { 'content-type': 'application/json' })
        .end(JSON.stringify({ error: 'Controlled backend failure' }))
      return
    }
    const bids = auction.imp
      .filter((imp) => !mode.empty && imp.id !== 'empty-slot')
      .map((imp) => ({
        id: `fixture-bid-${++sequence}`,
        impid: imp.id,
        price: 1.23,
        crid: 'fixture-creative',
        w: 300,
        h: 250,
        adm: `<div class="production-creative">Production fixture \${AUCTION_PRICE}</div><img class="production-asset" src="${settings.creativeOrigin}/creative-asset.svg?bid=${sequence}" width="30" height="30">`,
        adomain: ['example.com'],
        ext: { prebid: { type: 'banner', bidder: 'example-bidder' } },
      }))
    record({
      fixtureResponse: bids.length ? 'bid' : 'no-bid',
      auctionId: auction.id,
      status: 200,
      bidCount: bids.length,
    })
    response.setHeader('content-type', 'application/json')
    response.end(
      JSON.stringify({
        id: auction.id,
        cur: 'USD',
        seatbid: bids.length ? [{ seat: 'example-bidder', bid: bids }] : [],
      })
    )
    return
  }
  if (url.pathname === '/external-prebid.js') {
    response.setHeader('content-type', 'application/javascript')
    response.end(bundle)
    return
  }
  if (url.pathname === '/fixture-puc') {
    response.setHeader('content-type', 'text/html')
    response.end(pucPage(settings.publisherOrigin))
    return
  }
  if (url.pathname === '/fixture-puc.js') {
    response.setHeader('content-type', 'application/javascript')
    response.end(puc)
    return
  }
  if (url.pathname === '/creative-asset.svg') {
    response.setHeader('content-type', 'image/svg+xml')
    response.end(
      '<svg xmlns="http://www.w3.org/2000/svg" width="30" height="30"><rect width="30" height="30" fill="green"/></svg>'
    )
    return
  }
  if (
    ['/browser-direct', '/browser-publisher', '/browser-empty'].includes(
      url.pathname
    )
  ) {
    response.setHeader('content-type', 'text/html; charset=utf-8')
    response.end(page(url.pathname === '/browser-empty'))
    return
  }
  response.writeHead(404).end()
}
for (const listener of settings.listeners) {
  const server = listener.tls
    ? https.createServer(
        {
          key: readFileSync(`/work/${listener.certificate}.key`),
          cert: readFileSync(`/work/${listener.certificate}.pem`),
        },
        (req, res) => {
          handle(req, res).catch((error) => {
            record({ error: error.message })
            res.destroy(error)
          })
        }
      )
    : http.createServer((req, res) => {
        handle(req, res).catch((error) => res.destroy(error))
      })
  server.on('tlsClientError', (error, socket) =>
    record({
      tlsError: error.code,
      message: error.message,
      sni: socket.servername ?? null,
      port: listener.port,
      certificate: listener.certificate,
    })
  )
  server.requestTimeout = 10_000
  server.headersTimeout = 10_000
  await new Promise((resolve, reject) => {
    server.once('error', reject)
    server.listen(listener.port, '127.0.0.1', resolve)
  })
  servers.push(server)
}
writeFileSync('/work/origin.ready', 'ready')
process.on('SIGTERM', () => {
  for (const server of servers) server.closeAllConnections?.()
  for (const server of servers) server.close()
})
