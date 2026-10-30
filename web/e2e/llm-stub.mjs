// Minimal stand-in for an OpenAI-compatible LLM endpoint (Ollama, LM Studio), so E2E
// tests never need a real model. The server under test is pointed at it through the
// LLM settings (e2e/assistant.spec.ts). Answers are deterministic:
// - set_filter (natural-language search): a filter derived from keywords
// - chat with tools: first a search_catalog call, then a text answer naming the results
// - anything else: a short text
import { createServer } from 'node:http'

const port = Number(process.env.STUB_PORT ?? 19099)

function json(res, status, body) {
  res.writeHead(status, { 'Content-Type': 'application/json' })
  res.end(JSON.stringify(body))
}

function message(content, toolCalls) {
  return {
    id: 'stub',
    object: 'chat.completion',
    choices: [
      {
        index: 0,
        finish_reason: toolCalls ? 'tool_calls' : 'stop',
        message: { role: 'assistant', content: toolCalls ? null : content, tool_calls: toolCalls },
      },
    ],
  }
}

function call(name, args) {
  return [{ id: `call-${name}`, type: 'function', function: { name, arguments: JSON.stringify(args) } }]
}

/** "lustige Filme aus den 90ern unter 2 Stunden die ich noch nicht gesehen habe" → filter. */
function filterFor(text, genres) {
  const t = text.toLowerCase()
  const filter = {}
  if (t.includes('film')) filter.kinds = ['movie']
  if (t.includes('serie')) filter.kinds = ['series']
  const decade = t.match(/(\d0)er/)
  if (decade) {
    const start = Number(decade[1]) >= 30 ? 1900 + Number(decade[1]) : 2000 + Number(decade[1])
    filter.year_from = start
    filter.year_to = start + 9
  }
  const hours = t.match(/unter (\d+) stunden?/)
  if (hours) filter.max_runtime_minutes = Number(hours[1]) * 60
  if (t.includes('nicht gesehen')) filter.played = false
  if ((t.includes('lustig') || t.includes('komödie')) && genres.includes('Komödie')) filter.genres = ['Komödie']
  return filter
}

createServer((req, res) => {
  if (req.url === '/health') return json(res, 200, { ok: true })
  if (req.method !== 'POST' || !req.url?.endsWith('/chat/completions')) return json(res, 404, { error: 'not found' })
  let raw = ''
  req.on('data', (chunk) => (raw += chunk))
  req.on('end', () => {
    const body = JSON.parse(raw || '{}')
    const messages = body.messages ?? []
    const tools = (body.tools ?? []).map((t) => t.function?.name)
    const lastUser = [...messages].reverse().find((m) => m.role === 'user')?.content ?? ''
    if (tools.includes('set_filter')) {
      const genres = body.tools[0].function.parameters.properties.genres.items.enum ?? []
      return json(res, 200, message(null, call('set_filter', filterFor(lastUser, genres))))
    }
    if (tools.includes('search_catalog') && !messages.some((m) => m.role === 'tool')) {
      const spec = body.tools.find((t) => t.function.name === 'search_catalog').function
      const genres = spec.parameters.properties.genres.items.enum ?? []
      const args = { kinds: ['movie'], sort: 'title', limit: 10 }
      if (genres.includes('Komödie')) args.genres = ['Komödie']
      return json(res, 200, message(null, call('search_catalog', args)))
    }
    const toolResult = [...messages].reverse().find((m) => m.role === 'tool')
    if (toolResult) {
      const titles = (JSON.parse(toolResult.content).items ?? []).map((i) => i.title)
      return json(res, 200, message(`Ich habe gefunden: ${titles.join(', ') || 'nichts'}.`))
    }
    return json(res, 200, message('OK'))
  })
}).listen(port, '127.0.0.1')
