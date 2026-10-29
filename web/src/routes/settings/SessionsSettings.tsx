import clsx from 'clsx'
import { Film, Pause, Play } from 'lucide-react'
import { type ActiveSession, useActiveSessions } from '@/api/queries/playstate'
import { BlurImage } from '@/components/media/BlurImage'
import { Section } from '@/components/ui/Section'
import { formatTimecode, relativeTime } from '@/lib/format'
import { episodeLabel } from '@/lib/media'

const METHODS: Record<ActiveSession['play_method'], { label: string; className: string }> = {
  direct_play: { label: 'Direct Play', className: 'bg-emerald-500/15 text-emerald-400' },
  remux: { label: 'Remux', className: 'bg-sky-500/15 text-sky-400' },
  transcode: { label: 'Transcode', className: 'bg-amber-500/15 text-amber-400' },
}

/** Live list of what is being watched right now (administrators). */
export function SessionsSettings() {
  const sessions = useActiveSessions()
  const list = sessions.data ?? []

  return (
    <Section title="Aktive Wiedergaben" description="Wer gerade was schaut. Aktualisiert sich alle paar Sekunden.">
      {sessions.isSuccess && list.length === 0 && <p className="text-sm text-muted">Gerade läuft nichts.</p>}
      <ul className="divide-y divide-line/60" data-testid="session-list">
        {list.map((session) => (
          <SessionRow key={session.id} session={session} />
        ))}
      </ul>
    </Section>
  )
}

function SessionRow({ session }: { session: ActiveSession }) {
  const item = session.item
  const method = METHODS[session.play_method]
  const progress = session.duration_ms ? Math.min(1, session.position_ms / session.duration_ms) : null
  const title = item?.kind === 'episode' && item.series_title ? item.series_title : (item?.title ?? 'Unbekannter Eintrag')
  const subtitle = item?.kind === 'episode' ? [episodeLabel(item), item.title].filter(Boolean).join(' · ') : item?.year

  return (
    <li className="flex gap-4 py-3" data-testid="active-session">
      <div className="relative w-28 shrink-0 overflow-hidden rounded-lg bg-surface-2 md:w-36">
        {item?.backdrop_url || item?.poster_url ? (
          <BlurImage src={item.backdrop_url ?? item.poster_url} blurhash={null} alt="" className="aspect-video" />
        ) : (
          <div className="flex aspect-video items-center justify-center">
            <Film className="size-6 text-subtle" aria-hidden="true" />
          </div>
        )}
      </div>
      <div className="min-w-0 flex-1">
        <p className="flex flex-wrap items-center gap-2 text-sm font-semibold">
          <span className="truncate">{title}</span>
          <span className={clsx('rounded-full px-2 py-0.5 text-xs font-bold', method.className)} data-testid="session-method">
            {method.label}
          </span>
          {session.ffmpeg_running && <span className="text-xs text-subtle">ffmpeg aktiv</span>}
        </p>
        {subtitle && <p className="truncate text-xs text-muted">{subtitle}</p>}
        <p className="mt-1 text-xs text-subtle" data-testid="session-user">
          {session.user_name}
          {session.device_name && ` · ${session.device_name}`}
          {session.client && ` (${session.client})`}
        </p>
        <div className="mt-2 flex items-center gap-2 text-xs text-muted">
          {session.is_paused ? (
            <Pause className="size-3.5" aria-label="Pausiert" />
          ) : (
            <Play className="size-3.5 fill-current" aria-label="Läuft" />
          )}
          <span className="tabular-nums" data-testid="session-position">
            {formatTimecode(session.position_ms)}
            {session.duration_ms ? ` / ${formatTimecode(session.duration_ms)}` : ''}
          </span>
          {progress !== null && (
            <span className="h-1 w-24 overflow-hidden rounded-full bg-surface-3" aria-hidden="true">
              <span className="block h-full bg-accent" style={{ width: `${Math.round(progress * 100)}%` }} />
            </span>
          )}
          <span className="text-subtle">· zuletzt gemeldet {relativeTime(session.last_report_at)}</span>
        </div>
      </div>
    </li>
  )
}
