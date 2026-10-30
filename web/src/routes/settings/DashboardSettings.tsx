import { Link } from '@tanstack/react-router'
import clsx from 'clsx'
import { Activity, Cpu, HardDrive, MemoryStick, Pause, Play } from 'lucide-react'
import type { ReactNode } from 'react'
import { useDashboard } from '@/api/queries/ops'
import { Section } from '@/components/ui/Section'
import { formatBytes, formatRate } from '@/lib/bytes'
import { relativeTime } from '@/lib/format'

const METHODS: Record<string, string> = {
  direct_play: 'Direct Play',
  remux: 'Remux',
  transcode: 'Transcode',
}

function Stat({ icon, label, value, hint }: { icon: ReactNode; label: string; value: string; hint?: string }) {
  return (
    <div className="rounded-xl border border-line/60 bg-surface-2/60 p-4">
      <p className="flex items-center gap-2 text-xs font-semibold uppercase tracking-wide text-subtle">
        {icon}
        {label}
      </p>
      <p className="mt-2 text-2xl font-extrabold tabular-nums">{value}</p>
      {hint && <p className="mt-0.5 text-xs text-muted">{hint}</p>}
    </div>
  )
}

function Bar({ share }: { share: number }) {
  return (
    <span className="block h-1.5 w-full overflow-hidden rounded-full bg-surface-3" aria-hidden="true">
      <span
        className={clsx('block h-full', share > 0.9 ? 'bg-red-400' : 'bg-accent')}
        style={{ width: `${Math.round(Math.min(1, share) * 100)}%` }}
      />
    </span>
  )
}

/** Server overview: load, bandwidth per playback, storage and the job queue. */
export function DashboardSettings() {
  const dashboard = useDashboard()
  const d = dashboard.data
  if (!d) return null
  const memoryShare = d.memory_total_bytes ? d.memory_used_bytes / d.memory_total_bytes : 0

  return (
    <div className="flex flex-col gap-6" data-testid="dashboard">
      <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
        <Stat
          icon={<Cpu className="size-3.5" aria-hidden="true" />}
          label="CPU"
          value={`${Math.round(d.cpu_percent)} %`}
          hint={`Flixnet: ${Math.round(d.process_cpu_percent)} %`}
        />
        <Stat
          icon={<MemoryStick className="size-3.5" aria-hidden="true" />}
          label="Arbeitsspeicher"
          value={`${Math.round(memoryShare * 100)} %`}
          hint={`${formatBytes(d.memory_used_bytes)} von ${formatBytes(d.memory_total_bytes)} · Flixnet ${formatBytes(d.process_memory_bytes)}`}
        />
        <Stat
          icon={<Activity className="size-3.5" aria-hidden="true" />}
          label="Bandbreite"
          value={formatRate(d.bandwidth_bytes_per_second)}
          hint={`${formatBytes(d.bandwidth_total_bytes)} seit dem Start`}
        />
        <Stat
          icon={<Play className="size-3.5" aria-hidden="true" />}
          label="Wiedergaben"
          value={String(d.sessions.length)}
          hint={`${d.transcodes} Transcodes · Version ${d.version} · läuft ${relativeTime(d.started_at).replace(/^vor /, 'seit ')}`}
        />
      </div>

      <Section
        title="Wiedergaben & Bandbreite"
        description="Was gerade läuft und wie viele Daten dafür fließen."
        action={
          <Link to="/settings" search={{ tab: 'sessions' }} className="text-sm font-semibold text-accent hover:text-accent-hover">
            Alle Details
          </Link>
        }
      >
        {d.sessions.length === 0 && <p className="text-sm text-muted">Gerade läuft nichts.</p>}
        <ul className="divide-y divide-line/60" data-testid="dashboard-sessions">
          {d.sessions.map((s) => (
            <li key={s.session_id} className="flex flex-wrap items-center gap-3 py-3 text-sm">
              {s.is_paused ? (
                <Pause className="size-4 text-muted" aria-label="Pausiert" />
              ) : (
                <Play className="size-4 fill-current text-accent" aria-label="Läuft" />
              )}
              <div className="min-w-0 flex-1">
                <p className="truncate font-semibold">{s.title}</p>
                <p className="text-xs text-subtle">
                  {s.user_name} · {METHODS[s.play_method] ?? s.play_method}
                </p>
              </div>
              <div className="text-right tabular-nums">
                <p className="font-semibold">{formatRate(s.bytes_per_second)}</p>
                <p className="text-xs text-subtle">{formatBytes(s.bytes)}</p>
              </div>
            </li>
          ))}
        </ul>
      </Section>

      <Section title="Speicher" description="Belegung der Konfigurations- und Transcode-Ordner.">
        <ul className="flex flex-col gap-4">
          {d.storage.map((s) => {
            const used = s.disk_total_bytes && s.disk_available_bytes != null ? s.disk_total_bytes - s.disk_available_bytes : null
            return (
              <li key={s.label} className="flex flex-col gap-1.5">
                <p className="flex flex-wrap items-center gap-2 text-sm">
                  <HardDrive className="size-4 text-muted" aria-hidden="true" />
                  <span className="font-semibold">{s.label}</span>
                  <span className="truncate text-xs text-subtle">{s.path}</span>
                  <span className="ml-auto tabular-nums text-muted">Ordner: {formatBytes(s.used_bytes)}</span>
                </p>
                {s.disk_total_bytes != null && used != null && (
                  <>
                    <Bar share={used / s.disk_total_bytes} />
                    <p className="text-xs text-subtle">
                      Laufwerk: {formatBytes(s.disk_available_bytes ?? 0)} frei von {formatBytes(s.disk_total_bytes)}
                    </p>
                  </>
                )}
              </li>
            )
          })}
        </ul>
      </Section>

      <Section
        title="Aufgaben"
        description="Hintergrundaufgaben wie Scans, Analysen und Sicherungen."
        action={
          <Link to="/settings" search={{ tab: 'jobs' }} className="text-sm font-semibold text-accent hover:text-accent-hover">
            Zu den Aufgaben
          </Link>
        }
      >
        <div className="flex flex-wrap gap-6 text-sm">
          <p>
            <span className="text-2xl font-extrabold tabular-nums">{d.jobs.running}</span> <span className="text-muted">laufen</span>
          </p>
          <p>
            <span className="text-2xl font-extrabold tabular-nums">{d.jobs.queued}</span> <span className="text-muted">warten</span>
          </p>
          <p>
            <span className={clsx('text-2xl font-extrabold tabular-nums', d.jobs.failed > 0 && 'text-red-400')}>{d.jobs.failed}</span>{' '}
            <span className="text-muted">fehlgeschlagen</span>
          </p>
          <Link to="/settings" search={{ tab: 'logs' }} className="ml-auto self-center font-semibold text-accent hover:text-accent-hover">
            Protokoll ansehen
          </Link>
        </div>
      </Section>
    </div>
  )
}
