import { useQuery } from '@tanstack/react-query'
import clsx from 'clsx'
import { Pause, Play } from 'lucide-react'
import { useRef, useState } from 'react'
import { fetchLogs, type LogLevel, type LogLine } from '@/api/queries/ops'
import { Button } from '@/components/ui/Button'
import { Section, SelectField } from '@/components/ui/Section'

const LEVELS: { value: LogLevel; label: string }[] = [
  { value: 'error', label: 'Nur Fehler' },
  { value: 'warn', label: 'Warnungen und Fehler' },
  { value: 'info', label: 'Info (Standard)' },
  { value: 'debug', label: 'Debug' },
  { value: 'trace', label: 'Alles (Trace)' },
]

const LEVEL_STYLE: Record<LogLevel, string> = {
  error: 'text-red-400',
  warn: 'text-amber-400',
  info: 'text-sky-400',
  debug: 'text-subtle',
  trace: 'text-subtle',
}

const MAX_LINES = 2000
const time = new Intl.DateTimeFormat('de', { hour: '2-digit', minute: '2-digit', second: '2-digit' })

/** The last ~2000 log lines of the server, tailed live by polling. */
export function LogsSettings() {
  const [level, setLevel] = useState<LogLevel>('info')
  const [live, setLive] = useState(true)
  const [filter, setFilter] = useState('')
  const buffer = useRef<LogLine[]>([])
  const lastSeq = useRef(0)

  const logs = useQuery({
    queryKey: ['ops', 'logs', level],
    queryFn: async () => {
      const page = await fetchLogs(level, lastSeq.current)
      buffer.current = [...buffer.current, ...page.lines].slice(-MAX_LINES)
      lastSeq.current = page.last_seq
      return buffer.current
    },
    refetchInterval: live ? 2000 : false,
    gcTime: 0,
  })

  function changeLevel(value: string) {
    buffer.current = []
    lastSeq.current = 0
    setLevel(value as LogLevel)
  }

  const needle = filter.trim().toLowerCase()
  const lines = (logs.data ?? []).filter(
    (l) => !needle || l.message.toLowerCase().includes(needle) || l.target.toLowerCase().includes(needle),
  )

  return (
    <Section
      title="Protokoll"
      description="Die letzten Zeilen des Server-Logs. Neue Zeilen erscheinen automatisch."
      action={
        <Button variant="ghost" className="h-9 px-4" onClick={() => setLive((v) => !v)} aria-pressed={live}>
          {live ? <Pause className="size-4" aria-hidden="true" /> : <Play className="size-4" aria-hidden="true" />}
          {live ? 'Anhalten' : 'Fortsetzen'}
        </Button>
      }
    >
      <div className="mb-4 grid gap-3 md:grid-cols-2">
        <SelectField label="Mindest-Level" value={level} onChange={changeLevel} options={LEVELS} />
        <label className="flex flex-col gap-1.5">
          <span className="text-sm font-semibold text-muted">Filtern</span>
          <input
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            placeholder="z. B. scan oder ffmpeg"
            className="h-11 rounded-lg border border-line bg-surface-2 px-3.5 text-fg placeholder:text-subtle focus:border-accent focus:outline-none"
          />
        </label>
      </div>
      <div
        className="max-h-[32rem] overflow-auto rounded-lg border border-line/60 bg-canvas/80 p-3 font-mono text-xs leading-relaxed"
        data-testid="log-lines"
      >
        {lines.length === 0 && <p className="text-subtle">Keine passenden Zeilen.</p>}
        {lines.map((line) => (
          <p key={line.seq} className="whitespace-pre-wrap break-words">
            <span className="text-subtle">{time.format(line.timestamp)} </span>
            <span className={clsx('font-bold uppercase', LEVEL_STYLE[line.level])}>{line.level.padEnd(5)} </span>
            <span className="text-muted">{line.target}: </span>
            {line.message}
          </p>
        ))}
      </div>
    </Section>
  )
}
