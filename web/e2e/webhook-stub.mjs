// Receives webhook notifications for e2e/operations.spec.ts and lets the test read
// them back (GET /received). Never forwards anything.
import { createServer } from 'node:http'

const port = Number(process.env.STUB_PORT ?? 19099)
const received = []

createServer((req, res) => {
  const url = new URL(req.url ?? '/', `http://127.0.0.1:${port}`)
  if (url.pathname === '/health') {
    res.writeHead(200, { 'Content-Type': 'application/json' })
    return res.end('{"ok":true}')
  }
  if (url.pathname === '/received') {
    res.writeHead(200, { 'Content-Type': 'application/json', 'Access-Control-Allow-Origin': '*' })
    return res.end(JSON.stringify(received))
  }
  let body = ''
  req.on('data', (chunk) => (body += chunk))
  req.on('end', () => {
    received.push({
      path: url.pathname,
      event: req.headers['x-flixnet-event'] ?? null,
      signature: req.headers['x-flixnet-signature'] ?? null,
      body: body ? JSON.parse(body) : null,
    })
    res.writeHead(url.pathname === '/broken' ? 500 : 204)
    res.end()
  })
}).listen(port, '127.0.0.1')
