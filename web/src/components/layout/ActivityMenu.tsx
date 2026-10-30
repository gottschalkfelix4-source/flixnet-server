import { Link } from '@tanstack/react-router'
import clsx from 'clsx'
import { Activity } from 'lucide-react'
import { useCallback, useId, useRef, useState } from 'react'
import type { Schemas } from '@/api/client'
import { useServerActivity } from '@/api/queries/jobs'
import { useActiveSessions } from '@/api/queries/playstate'
import { useSystemInfo } from '@/api/queries/system'
import { activityTitle } from '@/lib/jobs'
import { useDismiss } from './useDismiss'

type Work = Schemas['ActivityDto']

/**
 * The activity indicator of the top bar, as in Plex: a grey pulse when idle, an
 * orange arc spinning around it while the server works, the icon and the number of
 * running streams in orange while something plays. The dropdown names the server,
 * links the dashboard (with the stream count) and lists the running tasks.
 */
export function ActivityMenu() {
  const [open, setOpen] = useState(false)
  const close = useCallback(() => setOpen(false), [])
  const root = useRef<HTMLDivElement>(null)
  useDismiss(root, open, close)
  const panelId = useId()
  const server = useSystemInfo().data?.name ?? 'Flixnet'
  const sessions = useActiveSessions({ interval: open ? 3000 : 10_000 })
  const activity = useServerActivity(true, open)
  const streams = sessions.data?.length ?? 0
  const work = activity.data ?? []
  const busy = work.some((w) => w.running > 0)

  const label = [
    'Aktivität',
    streams > 0 && `${streams} ${streams === 1 ? 'Wiedergabe' : 'Wiedergaben'}`,
    busy && 'Server arbeitet',
  ]
    .filter(Boolean)
    .join(', ')

  return (
    <div ref={root} className="relative">
      <button
        type="button"
        className={clsx(
          'flex h-10 items-center gap-1.5 rounded-full px-2 transition-colors hover:bg-surface-2',
          streams > 0 ? 'text-accent' : 'text-muted hover:text-fg',
        )}
        aria-label={label}
        title={label}
        aria-expanded={open}
        aria-controls={panelId}
        onClick={() => {
          // Opening shows the current state, not the last poll.
          if (!open) void Promise.all([sessions.refetch(), activity.refetch()])
          setOpen((o) => !o)
        }}
        data-testid="activity-button"
        data-busy={busy || undefined}
      >
        {streams > 0 && (
          <span className="text-sm font-bold tabular-nums" data-testid="activity-streams">
            {streams}
          </span>
        )}
        <span className="relative flex size-7 items-center justify-center">
          <Activity className="size-5" aria-hidden="true" />
          {busy && (
            <svg className="absolute inset-0 size-7 motion-safe:animate-spin" viewBox="0 0 28 28" aria-hidden="true">
              <circle
                cx="14"
                cy="14"
                r="12.5"
                fill="none"
                stroke="var(--color-accent)"
                strokeWidth={2}
                strokeLinecap="round"
                strokeDasharray="20 60"
              />
            </svg>
          )}
        </span>
      </button>

      {open && (
        <div
          id={panelId}
          role="dialog"
          aria-label="Aktivität"
          className="absolute top-12 right-0 z-50 w-[min(20rem,calc(100vw-1rem))] overflow-hidden rounded-lg border border-line bg-surface-2 py-2 shadow-2xl"
          data-testid="activity-panel"
        >
          <p className="px-4 pt-1 pb-2 text-sm font-extrabold tracking-wide text-subtle uppercase">{server}</p>
          <Link
            to="/settings"
            search={{ tab: 'dashboard' }}
            onClick={close}
            className="flex items-center px-4 py-2 text-sm font-semibold transition-colors hover:bg-surface-3"
          >
            Dashboard
            {streams > 0 && <span className="ml-auto font-bold text-accent tabular-nums">{streams}</span>}
          </Link>
          {work.length > 0 && (
            <ul aria-label="Server-Aufgaben">
              {work.map((w) => (
                <li key={`${w.kind}:${w.library_id ?? ''}`}>
                  <WorkRow work={w} onNavigate={close} />
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  )
}

/** Two grey lines (task, detail) and a small progress ring, like Plex' task rows. */
function WorkRow({ work: w, onNavigate }: { work: Work; onNavigate: () => void }) {
  const total = w.done + w.running + w.queued
  const detail = total > 1 ? `${w.done} von ${total} erledigt` : w.running > 0 ? 'Läuft' : 'Wartet'
  return (
    <Link
      to="/settings"
      search={{ tab: 'jobs' }}
      onClick={onNavigate}
      className="flex items-center gap-3 px-4 py-2 text-sm text-muted transition-colors hover:bg-surface-3 hover:text-fg"
      data-testid="activity-work"
    >
      <span className="min-w-0 flex-1">
        <span className="block truncate">{activityTitle(w)}:</span>
        <span className="block truncate tabular-nums">{detail}</span>
      </span>
      <ProgressRing progress={w.progress} running={w.running > 0} label={activityTitle(w)} />
    </Link>
  )
}

function ProgressRing({ progress, running, label }: { progress: number | null | undefined; running: boolean; label: string }) {
  const r = 7
  const circumference = 2 * Math.PI * r
  const known = progress != null
  return (
    <svg
      viewBox="0 0 18 18"
      className={clsx('size-4.5 shrink-0 -rotate-90', !known && running && 'motion-safe:animate-spin')}
      role="progressbar"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={known ? Math.round(progress * 100) : undefined}
    >
      <circle cx="9" cy="9" r={r} fill="none" stroke="var(--color-accent)" strokeOpacity={known ? 0.25 : 0} strokeWidth={2} />
      <circle
        cx="9"
        cy="9"
        r={r}
        fill="none"
        stroke="var(--color-accent)"
        strokeOpacity={running || known ? 1 : 0.4}
        strokeWidth={2}
        strokeLinecap="round"
        strokeDasharray={known ? `${Math.max(0.02, progress) * circumference} ${circumference}` : `${circumference / 4} ${circumference}`}
        className="transition-[stroke-dasharray]"
      />
    </svg>
  )
}
