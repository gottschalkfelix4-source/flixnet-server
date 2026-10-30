// Minimal stand-in for the OpenSubtitles.com REST API, so E2E tests never call the
// real service. The server under test is pointed at it via FLIXNET_OPENSUBTITLES_URL.
import { createServer } from 'node:http'

const port = Number(process.env.STUB_PORT ?? 18098)
const base = `http://127.0.0.1:${port}`

function json(res, status, body) {
  res.writeHead(status, { 'Content-Type': 'application/json' })
  res.end(JSON.stringify(body))
}

function result(fileId, language, release, downloads, hash) {
  return {
    id: String(fileId),
    type: 'subtitle',
    attributes: {
      language,
      download_count: downloads,
      hearing_impaired: false,
      hd: true,
      fps: 25,
      votes: 4,
      ratings: 8,
      from_trusted: true,
      release,
      moviehash_match: hash,
      files: [{ file_id: fileId, file_name: `${release}.srt` }],
    },
  }
}

createServer((req, res) => {
  const url = new URL(req.url ?? '/', base)
  if (url.pathname === '/health') return json(res, 200, { ok: true })
  if (req.headers['api-key'] !== 'e2e-key' && url.pathname.startsWith('/api/v1/')) {
    return json(res, 403, { message: 'invalid api key' })
  }
  if (url.pathname === '/api/v1/infos/formats') return json(res, 200, { data: { output_formats: ['srt'] } })
  if (url.pathname === '/api/v1/login') return json(res, 200, { token: 'e2e-token', status: 200 })
  if (url.pathname === '/api/v1/subtitles') {
    const language = url.searchParams.get('languages') ?? 'de'
    return json(res, 200, {
      total_count: 2,
      data: [
        result(7001, language, 'Untertiteltest.2021.German.1080p.WEB.x264-E2E', 321, false),
        result(7002, language, 'Ganz.Anderer.Film.720p', 5000, false),
      ],
    })
  }
  if (url.pathname === '/api/v1/download' && req.method === 'POST') {
    return json(res, 200, { link: `${base}/files/sub.srt`, file_name: 'sub.srt', remaining: 9 })
  }
  if (url.pathname === '/files/sub.srt') {
    res.writeHead(200, { 'Content-Type': 'application/x-subrip' })
    return res.end('1\n00:00:00,500 --> 00:00:02,000\nHallo aus dem Test\n')
  }
  json(res, 404, { message: 'not found' })
}).listen(port, '127.0.0.1')
