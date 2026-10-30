import { Link } from '@tanstack/react-router'
import { useVirtualizer } from '@tanstack/react-virtual'
import clsx from 'clsx'
import { Radio } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { type Channel, GUIDE_PAGE, type Program, formatClock, formatDay, useGuidePages } from '@/api/queries/livetv'

const ROW_H = 64
const HEADER_H = 44
const CHANNEL_W = 200
const SLOT_MS = 30 * 60_000
const SLOT_W = 180
const PX_PER_MS = SLOT_W / SLOT_MS
/** The grid covers an hour back and a day ahead. */
const BACK_MS = 60 * 60_000
const SPAN_MS = 25 * 60 * 60_000

/**
 * The program guide: channels as rows, time as columns, virtualized in both
 * directions — only visible rows are rendered (and their guide pages loaded), and
 * within them only programs in the visible time range. A line marks the current time.
 */
export function GuideGrid({
  channels,
  now,
  onProgram,
}: {
  channels: Channel[]
  now: number
  onProgram: (program: Program) => void
}) {
  // The window is fixed when the grid opens, so pages stay cached while scrolling.
  const [start] = useState(() => Math.floor(now / SLOT_MS) * SLOT_MS - BACK_MS)
  const end = start + SPAN_MS
  const slots = SPAN_MS / SLOT_MS
  const scrollRef = useRef<HTMLDivElement>(null)

  // The virtualizers re-render the grid on scroll themselves; nothing here is memoized.
  // eslint-disable-next-line react-hooks/incompatible-library
  const rows = useVirtualizer({
    count: channels.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_H,
    paddingStart: HEADER_H,
    overscan: 6,
  })
  const columns = useVirtualizer({
    horizontal: true,
    count: slots,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => SLOT_W,
    paddingStart: CHANNEL_W,
    overscan: 2,
  })

  // Open at the current time.
  useEffect(() => {
    const element = scrollRef.current
    if (element) element.scrollLeft = Math.max(0, (now - start) * PX_PER_MS - SLOT_W / 2)
    // Only once, when the grid appears.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  const visibleRows = rows.getVirtualItems()
  const visibleColumns = columns.getVirtualItems()
  const fromMs = start + (visibleColumns[0]?.index ?? 0) * SLOT_MS
  const toMs = start + ((visibleColumns.at(-1)?.index ?? slots - 1) + 1) * SLOT_MS

  const pages = [...new Set(visibleRows.map((row) => Math.floor(row.index / GUIDE_PAGE)))]
  const results = useGuidePages(pages, start, end)
  const programs = new Map<number, Program[]>()
  for (const result of results) {
    for (const row of result.data?.rows ?? []) programs.set(row.channel_id, row.programs)
  }

  const x = (ms: number) => CHANNEL_W + (Math.min(Math.max(ms, start), end) - start) * PX_PER_MS
  const width = CHANNEL_W + slots * SLOT_W

  return (
    <div
      ref={scrollRef}
      className="relative h-[calc(100dvh-13rem)] min-h-[24rem] overflow-auto rounded-[var(--radius-card)] border border-line/70 bg-surface/60"
      data-testid="guide-grid"
    >
      <div className="relative" style={{ width, height: rows.getTotalSize() }}>
        {/* Time header */}
        <div className="sticky top-0 z-20 flex border-b border-line/70 bg-surface" style={{ width, height: HEADER_H }}>
          <div
            className="sticky left-0 z-30 flex shrink-0 items-center border-r border-line/70 bg-surface px-4 text-xs font-bold text-muted"
            style={{ width: CHANNEL_W }}
          >
            {formatDay(fromMs)}
          </div>
          {visibleColumns.map((column) => (
            <div
              key={column.key}
              className="absolute top-0 flex h-full items-center border-l border-line/40 px-3 text-xs font-semibold text-muted"
              style={{ left: column.start, width: SLOT_W }}
            >
              {formatClock(start + column.index * SLOT_MS)}
            </div>
          ))}
        </div>

        {visibleRows.map((row) => {
          const channel = channels[row.index]
          if (!channel) return null
          const list = (programs.get(channel.id) ?? []).filter((p) => p.end_ms > fromMs && p.start_ms < toMs)
          return (
            <div
              key={channel.id}
              className="absolute left-0 flex border-b border-line/40"
              style={{ top: row.start, height: ROW_H, width }}
              data-testid="guide-row"
            >
              <Link
                to="/live/$channelId"
                params={{ channelId: String(channel.id) }}
                className="sticky left-0 z-10 flex shrink-0 items-center gap-3 border-r border-line/70 bg-surface px-3 hover:bg-surface-2"
                style={{ width: CHANNEL_W }}
                aria-label={`${channel.name} ansehen`}
              >
                <ChannelLogo channel={channel} />
                <span className="min-w-0">
                  {channel.number && <span className="block text-[11px] font-bold text-subtle">{channel.number}</span>}
                  <span className="block truncate text-sm font-bold">{channel.name}</span>
                </span>
              </Link>
              {list.map((program) => {
                const running = program.start_ms <= now && now < program.end_ms
                const left = x(program.start_ms)
                return (
                  <button
                    key={program.id}
                    type="button"
                    onClick={() => onProgram(program)}
                    className={clsx(
                      'absolute top-1 bottom-1 overflow-hidden rounded-lg border px-3 text-left transition-colors',
                      running
                        ? 'border-accent/60 bg-accent/10 hover:bg-accent/20'
                        : program.end_ms <= now
                          ? 'border-line/40 bg-surface-2/40 text-muted hover:bg-surface-2'
                          : 'border-line/60 bg-surface-2 hover:bg-surface-3',
                    )}
                    style={{ left: left + 2, width: Math.max(4, x(program.end_ms) - left - 4) }}
                    title={program.title}
                    data-testid="guide-program"
                  >
                    <span className="flex items-center gap-1.5 truncate text-sm font-semibold">
                      {program.recording && program.recording.status !== 'failed' && (
                        <span
                          className={clsx(
                            'size-2 shrink-0 rounded-full',
                            program.recording.conflict ? 'bg-amber-400' : 'bg-red-500',
                          )}
                          data-testid="guide-recording"
                          title={program.recording.conflict ? 'Aufnahme mit Konflikt' : 'Wird aufgenommen'}
                        />
                      )}
                      <span className="truncate">{program.title}</span>
                    </span>
                    <span className="block truncate text-xs text-subtle">
                      {formatClock(program.start_ms)}
                      {program.episode_label ? ` · ${program.episode_label}` : ''}
                    </span>
                  </button>
                )
              })}
            </div>
          )
        })}

        {now >= start && now < end && (
          <div
            className="pointer-events-none absolute top-0 z-10 w-0.5 bg-accent shadow-[0_0_8px_var(--color-accent)]"
            style={{ left: x(now), height: rows.getTotalSize() }}
            data-testid="guide-now-line"
            aria-hidden="true"
          />
        )}
      </div>
    </div>
  )
}

export function ChannelLogo({ channel, size = 'md' }: { channel: Channel; size?: 'md' | 'lg' }) {
  const [broken, setBroken] = useState(false)
  const box = size === 'lg' ? 'h-12 w-16' : 'h-9 w-12'
  if (!channel.logo_url || broken) {
    return (
      <span className={clsx('flex shrink-0 items-center justify-center rounded-md bg-surface-3 text-subtle', box)}>
        <Radio className="size-4" aria-hidden="true" />
      </span>
    )
  }
  return (
    <img
      src={channel.logo_url}
      alt=""
      loading="lazy"
      referrerPolicy="no-referrer"
      onError={() => setBroken(true)}
      className={clsx('shrink-0 rounded-md object-contain', box)}
    />
  )
}
