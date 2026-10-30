import { Link } from '@tanstack/react-router'
import clsx from 'clsx'
import { Activity, ArrowDownRight, Cpu, HardDrive, MemoryStick, Pause, Play } from 'lucide-react'
import { type ReactNode, useState } from 'react'
import type { Schemas } from '@/api/client'
import { type BandwidthRange, useDashboard, useDashboardBandwidth, useDashboardStats } from '@/api/queries/ops'
import { BandwidthChart } from '@/components/dashboard/BandwidthChart'
import { HistoryChart } from '@/components/dashboard/HistoryChart'
import { BlurImage } from '@/components/media/BlurImage'
import { Avatar } from '@/components/ui/Avatar'
import { formatBytes, formatRate } from '@/lib/bytes'
import { formatTimecode, formatWatchTime, relativeTime } from '@/lib/format'
import { episodeLabel } from '@/lib/media'

type NowPlaying = Schemas['SessionBandwidthDto']
type StreamLine = Schemas['StreamLineDto']
type TopUser = Schemas['TopUserDto']
type TopItem = Schemas['TopItemDto']

const METHODS: Record<string, string> = {
  direct_play: 'Direct Play',
  remux: 'Remux',
  transcode: 'Transcode',
}

const selectClass =
  'h-8 rounded-md border border-line bg-surface-2 px-2 text-xs font-semibold text-muted focus:border-accent focus:outline-none'

/** A dashboard block: small uppercase title with its controls, then the content. */
function Block({ title, controls, children, testId }: { title: string; controls?: ReactNode; children: ReactNode; testId?: string }) {
  return (
    <section className="border-t border-line/50 pt-5 pb-8" aria-label={title} data-testid={testId}>
      <header className="mb-4 flex flex-wrap items-center gap-3">
        <h2 className="text-sm font-extrabold tracking-[0.08em] uppercase">{title}</h2>
        <div className="ml-auto flex flex-wrap gap-2">{controls}</div>
      </header>
      {children}
    </section>
  )
}

function PeriodSelect({ value, onChange, options, label }: { value: number; onChange: (v: number) => void; options: number[]; label: string }) {
  return (
    <select aria-label={label} className={selectClass} value={value} onChange={(e) => onChange(Number(e.target.value))}>
      {options.map((d) => (
        <option key={d} value={d}>
          {d === 365 ? 'Letztes Jahr' : `Letzte ${d} Tage`}
        </option>
      ))}
    </select>
  )
}

/** Plex-style server dashboard: what plays, bandwidth, who watches what, and load. */
export function DashboardSettings() {
  const dashboard = useDashboard()
  const [range, setRange] = useState<BandwidthRange>('realtime')
  const bandwidth = useDashboardBandwidth(range)
  const [userDays, setUserDays] = useState(7)
  const userStats = useDashboardStats(userDays)
  const [historyDays, setHistoryDays] = useState(30)
  const historyStats = useDashboardStats(historyDays)
  const d = dashboard.data
  if (!d) return null

  return (
    <div className="flex flex-col" data-testid="dashboard">
      <Block title="Jetzt läuft" testId="now-playing">
        {d.sessions.length === 0 ? (
          <p className="text-sm text-muted">Gerade läuft nichts.</p>
        ) : (
          <ul className="grid gap-4 sm:grid-cols-[repeat(auto-fill,minmax(19rem,1fr))]" data-testid="dashboard-sessions">
            {d.sessions.map((s) => (
              <NowPlayingCard key={s.session_id} session={s} />
            ))}
          </ul>
        )}
      </Block>

      <Block
        title="Bandbreite"
        controls={
          <select aria-label="Zeitraum der Bandbreite" className={selectClass} value={range} onChange={(e) => setRange(e.target.value as BandwidthRange)}>
            <option value="realtime">Echtzeit</option>
            <option value="hour">Letzte Stunde</option>
            <option value="day">Letzte 24 Stunden</option>
          </select>
        }
      >
        <p className="mb-3 text-sm text-muted">
          Gerade <span className="font-semibold text-fg tabular-nums">{formatRate(d.bandwidth_bytes_per_second)}</span> ·{' '}
          {formatBytes(d.bandwidth_total_bytes)} seit dem Start
        </p>
        {bandwidth.data && <BandwidthChart points={bandwidth.data.points} slotSeconds={bandwidth.data.slot_seconds} />}
      </Block>

      <Block
        title="Top-Nutzer"
        controls={<PeriodSelect label="Zeitraum der Top-Nutzer" value={userDays} onChange={setUserDays} options={[7, 30, 90]} />}
        testId="top-users"
      >
        {userStats.data?.top_users.length === 0 && <p className="text-sm text-muted">In diesem Zeitraum wurde nichts geschaut.</p>}
        <ul className="grid gap-4 sm:grid-cols-[repeat(auto-fill,minmax(13rem,1fr))]">
          {userStats.data?.top_users.map((u) => (
            <TopUserCard key={u.user_id} user={u} />
          ))}
        </ul>
      </Block>

      <Block
        title="Wiedergabeverlauf"
        controls={<PeriodSelect label="Zeitraum des Verlaufs" value={historyDays} onChange={setHistoryDays} options={[14, 30, 90, 365]} />}
        testId="play-history"
      >
        {historyStats.data && <HistoryChart buckets={historyStats.data.history} bucketMs={historyStats.data.bucket_ms} />}
      </Block>

      <Block
        title="Meistgespielt"
        controls={<PeriodSelect label="Zeitraum der Bestenliste" value={historyDays} onChange={setHistoryDays} options={[14, 30, 90, 365]} />}
        testId="top-played"
      >
        <div className="grid gap-4 md:grid-cols-3">
          <TopColumn title="Filme" items={historyStats.data?.top_movies} />
          <TopColumn title="Serien" items={historyStats.data?.top_series} />
          <TopColumn title="Künstler" items={historyStats.data?.top_artists} round />
        </div>
      </Block>

      <ServerBlock d={d} />
    </div>
  )
}

function NowPlayingCard({ session: s }: { session: NowPlaying }) {
  const item = s.item
  const heading = item?.kind === 'episode' && item.series_title ? item.series_title : s.title
  const detail = item?.kind === 'episode' ? [episodeLabel(item), item.title].filter(Boolean).join(' · ') : item?.year
  const progress = s.duration_ms ? Math.min(1, s.position_ms / s.duration_ms) : 0
  return (
    <li className="overflow-hidden rounded-xl bg-surface-2 shadow-[var(--shadow-card)]" data-testid="now-playing-card">
      <div className="flex gap-3 p-3">
        <div className="w-16 shrink-0 overflow-hidden rounded-md">
          <BlurImage src={item?.poster_url} blurhash={item?.poster_blurhash} alt="" className="aspect-[2/3]" />
        </div>
        <div className="min-w-0">
          <p className="truncate font-bold">{heading}</p>
          {detail && <p className="truncate text-xs text-muted">{detail}</p>}
          {s.duration_ms != null && s.duration_ms >= 60_000 && <p className="mt-0.5 text-xs text-subtle">{formatWatchTime(s.duration_ms)}</p>}
        </div>
      </div>
      <div className="bg-accent/10 px-3 py-2 text-xs">
        <p className="flex items-center gap-2 font-semibold">
          {s.is_paused ? <Pause className="size-3.5" aria-hidden="true" /> : <Play className="size-3.5 fill-current" aria-hidden="true" />}
          <span className="truncate">{[s.client, s.device_name].filter(Boolean).join(' — ') || 'Unbekanntes Gerät'}</span>
        </p>
        <p className="mt-0.5 text-muted tabular-nums">
          {s.is_paused ? 'Pausiert' : 'Läuft'} — {formatTimecode(s.position_ms)}
          {s.duration_ms != null && ` / ${formatTimecode(s.duration_ms)}`}
        </p>
        <span className="mt-2 block h-1 overflow-hidden rounded-full bg-black/40" aria-hidden="true">
          <span className="block h-full bg-accent" style={{ width: `${Math.round(progress * 100)}%` }} />
        </span>
      </div>
      <dl className="grid grid-cols-[4.5rem_1fr] gap-y-1.5 px-3 py-2.5 text-xs">
        {s.video && <StreamRow label="Video" line={s.video} />}
        {s.audio && <StreamRow label="Audio" line={s.audio} />}
        <dt className="text-subtle">Bandbreite</dt>
        <dd className="tabular-nums">
          {formatRate(s.bytes_per_second)} <span className="text-subtle">· {METHODS[s.play_method] ?? s.play_method}</span>
        </dd>
      </dl>
      <div className="flex items-center gap-2 border-t border-line/50 px-3 py-2 text-xs">
        <Avatar id={s.user_id} name={s.user_name} size="sm" />
        <span className="font-semibold">{s.user_name}</span>
      </div>
    </li>
  )
}

function StreamRow({ label, line }: { label: string; line: StreamLine }) {
  const action = line.action === 'transcode' ? 'Transcode' : line.action === 'copy' ? 'Direct Stream' : 'Direct Play'
  return (
    <>
      <dt className="text-subtle">{label}</dt>
      <dd className="min-w-0">
        <p className="truncate">{line.source}</p>
        {line.action !== 'direct' && (
          <p className="flex items-center gap-1 truncate text-muted">
            <ArrowDownRight className="size-3 shrink-0" aria-hidden="true" />
            {line.target} — {action}
            {line.hardware && <span className="rounded bg-emerald-500/15 px-1 font-semibold text-emerald-300">{line.hardware}</span>}
          </p>
        )}
      </dd>
    </>
  )
}

function TopUserCard({ user: u }: { user: TopUser }) {
  const rows = [
    { label: 'Filme', ms: u.movies_ms },
    { label: 'Serien', ms: u.tv_ms },
    { label: 'Musik', ms: u.music_ms },
    { label: 'Sonstiges', ms: u.other_ms },
  ]
  return (
    <li className="overflow-hidden rounded-xl bg-surface-2" data-testid="top-user">
      <div className="flex items-center gap-3 p-3">
        <Avatar id={u.user_id} name={u.name} />
        <div className="min-w-0">
          <p className="truncate font-bold">{u.name}</p>
          <p className="text-xs text-muted tabular-nums">
            {u.plays} {u.plays === 1 ? 'Wiedergabe' : 'Wiedergaben'} · {formatWatchTime(u.watch_ms)}
          </p>
        </div>
      </div>
      <dl className="divide-y divide-line/50 border-t border-line/50 text-xs">
        {rows.map((r) => (
          <div key={r.label} className="flex px-3 py-1.5">
            <dt className={clsx('font-semibold', r.ms === 0 && 'text-subtle')}>{r.label}</dt>
            <dd className="ml-auto text-muted tabular-nums">{formatWatchTime(r.ms)}</dd>
          </div>
        ))}
      </dl>
    </li>
  )
}

function TopColumn({ title, items, round = false }: { title: string; items?: TopItem[]; round?: boolean }) {
  const lead = items?.[0]?.item
  return (
    <div className="overflow-hidden rounded-xl bg-surface-2" data-testid="top-column">
      <div className="relative flex h-20 items-center justify-center overflow-hidden">
        <BlurImage
          src={lead?.backdrop_url ?? lead?.poster_url}
          blurhash={null}
          alt=""
          className="absolute inset-0 opacity-40 blur-[1px]"
        />
        <h3 className="relative text-sm font-extrabold tracking-[0.12em] uppercase">{title}</h3>
      </div>
      {items && items.length === 0 && <p className="px-3 py-4 text-xs text-subtle">Noch nichts gespielt.</p>}
      <ol className="divide-y divide-line/50">
        {items?.map((t) => (
          <li key={t.item.id}>
            <Link to="/item/$id" params={{ id: String(t.item.id) }} className="flex items-center gap-3 px-3 py-2 hover:bg-surface-3">
              <span className={clsx('w-10 shrink-0 overflow-hidden', round ? 'rounded-full' : 'rounded')}>
                <BlurImage src={t.item.poster_url} blurhash={t.item.poster_blurhash} alt="" className={round ? 'aspect-square' : 'aspect-[2/3]'} />
              </span>
              <span className="min-w-0">
                <span className="block truncate text-sm font-semibold">{t.item.title}</span>
                <span className="block text-xs text-muted tabular-nums">
                  {t.plays} {t.plays === 1 ? 'Wiedergabe' : 'Wiedergaben'} · {t.users} {t.users === 1 ? 'Nutzer' : 'Nutzer'}
                </span>
              </span>
            </Link>
          </li>
        ))}
      </ol>
    </div>
  )
}

function Meter({ share }: { share: number }) {
  return (
    <span className="block h-1.5 w-full overflow-hidden rounded-full bg-surface-3" aria-hidden="true">
      <span
        className={clsx('block h-full', share > 0.9 ? 'bg-red-400' : 'bg-accent')}
        style={{ width: `${Math.round(Math.min(1, share) * 100)}%` }}
      />
    </span>
  )
}

function Stat({ icon, label, value, hint }: { icon: ReactNode; label: string; value: string; hint?: string }) {
  return (
    <div className="rounded-xl bg-surface-2 p-4">
      <p className="flex items-center gap-2 text-xs font-semibold tracking-wide text-subtle uppercase">
        {icon}
        {label}
      </p>
      <p className="mt-2 text-2xl font-extrabold tabular-nums">{value}</p>
      {hint && <p className="mt-0.5 text-xs text-muted">{hint}</p>}
    </div>
  )
}

function ServerBlock({ d }: { d: Schemas['DashboardDto'] }) {
  const memoryShare = d.memory_total_bytes ? d.memory_used_bytes / d.memory_total_bytes : 0
  return (
    <Block
      title="Server"
      controls={
        <Link to="/settings" search={{ tab: 'logs' }} className="text-xs font-semibold text-accent hover:text-accent-hover">
          Protokoll ansehen
        </Link>
      }
    >
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
          label="Transcodes"
          value={String(d.transcodes)}
          hint={`${d.sessions.length} Wiedergaben · Version ${d.version}`}
        />
        <Stat
          icon={<Play className="size-3.5" aria-hidden="true" />}
          label="Aufgaben"
          value={`${d.jobs.running} laufen`}
          hint={`${d.jobs.queued} warten · ${d.jobs.failed} fehlgeschlagen · läuft ${relativeTime(d.started_at).replace(/^vor /, 'seit ')}`}
        />
      </div>
      <ul className="mt-5 flex flex-col gap-4">
        {d.storage.map((s) => {
          const used = s.disk_total_bytes && s.disk_available_bytes != null ? s.disk_total_bytes - s.disk_available_bytes : null
          return (
            <li key={s.label} className="flex flex-col gap-1.5">
              <p className="flex items-center gap-2 text-sm">
                <HardDrive className="size-4 shrink-0 text-muted" aria-hidden="true" />
                <span className="font-semibold">{s.label}</span>
                <span className="min-w-0 flex-1 truncate text-xs text-subtle" title={s.path}>
                  {s.path}
                </span>
                <span className="shrink-0 text-muted tabular-nums">Ordner: {formatBytes(s.used_bytes)}</span>
              </p>
              {s.disk_total_bytes != null && used != null && (
                <>
                  <Meter share={used / s.disk_total_bytes} />
                  <p className="text-xs text-subtle">
                    Laufwerk: {formatBytes(s.disk_available_bytes ?? 0)} frei von {formatBytes(s.disk_total_bytes)}
                  </p>
                </>
              )}
            </li>
          )
        })}
      </ul>
    </Block>
  )
}
