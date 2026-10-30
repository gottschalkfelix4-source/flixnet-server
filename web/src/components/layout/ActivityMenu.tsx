import { Link } from '@tanstack/react-router'
import clsx from 'clsx'
import { Activity, ChevronRight, Pause } from 'lucide-react'
import { useEffect, useId, useRef, useState } from 'react'
import type { Schemas } from '@/api/client'
import { useServerActivity } from '@/api/queries/jobs'
import { useActiveSessions } from '@/api/queries/playstate'
import { BlurImage } from '@/components/media/BlurImage'
import { Avatar } from '@/components/ui/Avatar'
import { activityTitle } from '@/lib/jobs'
import { episodeLabel } from '@/lib/media'

type Stream = Schemas['ActiveSessionDto']
type Work = Schemas['ActivityDto']

/**
 * The activity bubble of the top bar (like Plex): a spinning ring while the server
 * scans, analyses or refreshes, a badge with the number of running streams, and on
 * click a panel with what plays and what the server is working on.
 */
export function ActivityMenu({ className }: { className: string }) {
  const [open, setOpen] = useState(false)
  const root = useRef<HTMLDivElement>(null)
  const panelId = useId()
  const streams = useActiveSessions({ interval: open ? 3000 : 10_000 }).data ?? []
  const work = useServerActivity(true).data ?? []
  const busy = work.some((w) => w.running > 0)

  // Close on outside click and Escape, like a menu.
  useEffect(() => {
    if (!open) return
    const onPointer = (e: PointerEvent) => {
      if (!root.current?.contains(e.target as Node)) setOpen(false)
    }
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && setOpen(false)
    document.addEventListener('pointerdown', onPointer)
    document.addEventListener('keydown', onKey)
    return () => {
      document.removeEventListener('pointerdown', onPointer)
      document.removeEventListener('keydown', onKey)
    }
  }, [open])

  const label = [
    'Aktivität',
    streams.length > 0 && `${streams.length} ${streams.length === 1 ? 'Wiedergabe' : 'Wiedergaben'}`,
    busy && 'Server arbeitet',
  ]
    .filter(Boolean)
    .join(', ')

  return (
    <div ref={root} className="pointer-events-auto relative">
      <button
        type="button"
        className={className}
        aria-label={label}
        title={label}
        aria-expanded={open}
        aria-controls={panelId}
        onClick={() => setOpen((o) => !o)}
        data-testid="activity-button"
        data-busy={busy || undefined}
      >
        <Activity className="size-5" aria-hidden="true" />
        {busy && (
          <svg className="absolute inset-0 size-10 motion-safe:animate-spin" viewBox="0 0 40 40" aria-hidden="true">
            <circle cx="20" cy="20" r="18" fill="none" stroke="currentColor" strokeOpacity={0.15} strokeWidth={2} />
            <circle
              cx="20"
              cy="20"
              r="18"
              fill="none"
              stroke="var(--color-accent)"
              strokeWidth={2}
              strokeLinecap="round"
              strokeDasharray="28 85"
            />
          </svg>
        )}
        {streams.length > 0 && (
          <span
            className="absolute -top-1 -right-1 flex h-5 min-w-5 items-center justify-center rounded-full bg-accent px-1 text-[11px] font-bold text-accent-fg tabular-nums"
            data-testid="activity-streams"
          >
            {streams.length}
          </span>
        )}
      </button>

      {open && (
        <div
          id={panelId}
          role="dialog"
          aria-label="Aktivität"
          className="absolute top-12 right-0 z-50 flex max-h-[min(36rem,80dvh)] w-[min(24rem,calc(100vw-2rem))] flex-col overflow-hidden rounded-xl border border-line bg-surface-2/95 shadow-2xl backdrop-blur-xl"
          data-testid="activity-panel"
        >
          <div className="overflow-y-auto">
            {streams.length === 0 && work.length === 0 && (
              <p className="px-4 py-8 text-center text-sm text-muted">Gerade ist nichts los.</p>
            )}
            {streams.length > 0 && (
              <section aria-label="Jetzt läuft" className="py-2">
                <h2 className="px-4 py-1.5 text-[11px] font-bold tracking-[0.12em] text-subtle uppercase">Jetzt läuft</h2>
                <ul>
                  {streams.map((s) => (
                    <StreamRow key={s.id} stream={s} onNavigate={() => setOpen(false)} />
                  ))}
                </ul>
              </section>
            )}
            {work.length > 0 && (
              <section aria-label="Server" className={clsx('py-2', streams.length > 0 && 'border-t border-line/60')}>
                <h2 className="px-4 py-1.5 text-[11px] font-bold tracking-[0.12em] text-subtle uppercase">Server</h2>
                <ul>
                  {work.map((w) => (
                    <WorkRow key={`${w.kind}:${w.library_id ?? ''}`} work={w} />
                  ))}
                </ul>
              </section>
            )}
          </div>
          <Link
            to="/settings"
            search={{ tab: 'dashboard' }}
            onClick={() => setOpen(false)}
            className="flex items-center justify-between border-t border-line/60 px-4 py-3 text-sm font-semibold text-muted transition-colors hover:bg-surface-3 hover:text-fg"
          >
            Dashboard öffnen
            <ChevronRight className="size-4" aria-hidden="true" />
          </Link>
        </div>
      )}
    </div>
  )
}

function StreamRow({ stream: s, onNavigate }: { stream: Stream; onNavigate: () => void }) {
  const item = s.item
  const title = item?.kind === 'episode' && item.series_title ? item.series_title : (item?.title ?? 'Unbekannt')
  const detail = item?.kind === 'episode' ? [episodeLabel(item), item.title].filter(Boolean).join(' · ') : item?.year
  const progress = s.duration_ms ? Math.min(1, s.position_ms / s.duration_ms) : 0
  return (
    <li>
      <Link
        to="/settings"
        search={{ tab: 'dashboard' }}
        onClick={onNavigate}
        className="flex gap-3 px-4 py-2 transition-colors hover:bg-surface-3"
        data-testid="activity-stream"
      >
        <span className="w-10 shrink-0 overflow-hidden rounded">
          <BlurImage src={item?.poster_url} blurhash={item?.poster_blurhash} alt="" className="aspect-[2/3]" />
        </span>
        <span className="min-w-0 flex-1">
          <span className="block truncate text-sm font-semibold">{title}</span>
          {detail && <span className="block truncate text-xs text-muted">{detail}</span>}
          <span className="mt-1 flex items-center gap-1.5 text-xs text-muted">
            <Avatar id={s.user_id} name={s.user_name} size="xs" />
            <span className="truncate">
              {s.user_name} · {s.device_name ?? s.client ?? 'Unbekanntes Gerät'}
            </span>
            {s.is_paused && <Pause className="ml-auto size-3 shrink-0" aria-label="Pausiert" />}
          </span>
          <span className="mt-1.5 block h-0.5 overflow-hidden rounded-full bg-black/40" aria-hidden="true">
            <span className="block h-full bg-accent" style={{ width: `${Math.round(progress * 100)}%` }} />
          </span>
        </span>
      </Link>
    </li>
  )
}

function WorkRow({ work: w }: { work: Work }) {
  const total = w.done + w.running + w.queued
  const count = total > 1 ? `${w.done} von ${total}` : w.running > 0 ? 'Läuft' : 'Wartet'
  return (
    <li className="px-4 py-2" data-testid="activity-work">
      <p className="flex items-baseline gap-3 text-sm">
        <span className="min-w-0 flex-1 truncate font-semibold">{activityTitle(w)}</span>
        <span className="shrink-0 text-xs text-muted tabular-nums">{count}</span>
      </p>
      <span
        className="mt-1.5 block h-1 overflow-hidden rounded-full bg-surface-3"
        role="progressbar"
        aria-label={activityTitle(w)}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={w.progress != null ? Math.round(w.progress * 100) : undefined}
      >
        {w.progress != null ? (
          <span className="block h-full rounded-full bg-accent transition-[width]" style={{ width: `${Math.round(w.progress * 100)}%` }} />
        ) : w.running > 0 ? (
          <span className="block h-full w-2/5 rounded-full bg-accent motion-safe:animate-indeterminate" />
        ) : null}
      </span>
    </li>
  )
}
