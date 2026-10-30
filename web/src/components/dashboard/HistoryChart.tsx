import { useRef, useState } from 'react'
import type { Schemas } from '@/api/client'
import { formatWatchTime } from '@/lib/format'
import { Legend, Tooltip, TooltipRow } from './chart'
import { SERIES, niceTicks, useWidth } from './scale'

type Bucket = Schemas['HistoryBucketDto']

const HEIGHT = 220
const PAD = { top: 12, right: 12, bottom: 26, left: 64 }
const GAP = 2

const KINDS = [
  { key: 'movies_ms', label: 'Filme', color: SERIES.movies },
  { key: 'tv_ms', label: 'Serien', color: SERIES.tv },
  { key: 'music_ms', label: 'Musik', color: SERIES.music },
  { key: 'other_ms', label: 'Sonstiges', color: SERIES.other },
] as const

const day = new Intl.DateTimeFormat('de', { day: 'numeric', month: 'short' })

/** Watch time per period as stacked bars (one segment per media kind). */
export function HistoryChart({ buckets, bucketMs }: { buckets: Bucket[]; bucketMs: number }) {
  const box = useRef<HTMLDivElement>(null)
  const width = useWidth(box)
  const [hover, setHover] = useState<number | null>(null)
  const plotW = Math.max(0, width - PAD.left - PAD.right)
  const plotH = HEIGHT - PAD.top - PAD.bottom
  const total = (b: Bucket) => KINDS.reduce((sum, k) => sum + b[k.key], 0)
  const ticks = niceTicks(Math.max(60_000, ...buckets.map(total)))
  const top = ticks[ticks.length - 1]!
  const slot = buckets.length ? plotW / buckets.length : plotW
  const barW = Math.max(4, Math.min(64, slot * 0.6))
  const y = (v: number) => PAD.top + plotH - (v / top) * plotH
  // Only kinds that occur get a legend entry (a single kind needs none: the title names it).
  const present = KINDS.filter((k) => buckets.some((b) => b[k.key] > 0))
  const label = (b: Bucket) =>
    bucketMs > 24 * 3600_000 ? `${day.format(b.start)} – ${day.format(b.start + bucketMs - 1)}` : day.format(b.start)
  const labelEvery = Math.max(1, Math.ceil(buckets.length / 8))
  const hovered = hover != null ? buckets[hover] : undefined

  return (
    <div className="flex flex-col gap-3">
      {present.length > 1 && <Legend items={present.map((k) => ({ label: k.label, color: k.color }))} />}
      <div ref={box} className="relative" data-testid="history-chart">
        {width > 0 && (
          <svg width={width} height={HEIGHT} role="img" aria-label="Wiedergabezeit pro Zeitraum nach Medienart">
            {ticks.map((t) => (
              <g key={t}>
                <line x1={PAD.left} x2={PAD.left + plotW} y1={y(t)} y2={y(t)} stroke="var(--color-line)" strokeWidth={1} />
                <text x={PAD.left - 8} y={y(t) + 4} textAnchor="end" className="fill-subtle text-[11px] tabular-nums">
                  {formatWatchTime(t)}
                </text>
              </g>
            ))}
            {buckets.map((b, i) => {
              const cx = PAD.left + slot * i + slot / 2
              let base = 0
              const segments = KINDS.filter((k) => b[k.key] > 0)
              return (
                <g key={b.start}>
                  {segments.map((k, s) => {
                    const from = y(base)
                    base += b[k.key]
                    const to = y(base)
                    // 2px surface gap between stacked segments; only the top one is rounded.
                    const h = Math.max(1, from - to - (s > 0 ? GAP : 0))
                    const isTop = s === segments.length - 1
                    return (
                      <path
                        key={k.key}
                        d={roundedTop(cx - barW / 2, to, barW, h, isTop ? Math.min(4, h, barW / 2) : 0)}
                        fill={k.color}
                        opacity={hover == null || hover === i ? 1 : 0.55}
                      />
                    )
                  })}
                  {(i % labelEvery === 0 || i === buckets.length - 1) && (
                    <text x={cx} y={HEIGHT - 8} textAnchor="middle" className="fill-subtle text-[11px]">
                      {day.format(b.start)}
                    </text>
                  )}
                  {/* Hit target: the whole column, bigger than the bar. */}
                  <rect
                    x={cx - slot / 2}
                    y={PAD.top}
                    width={slot}
                    height={plotH}
                    fill="transparent"
                    onPointerEnter={() => setHover(i)}
                    onPointerLeave={() => setHover(null)}
                  />
                </g>
              )
            })}
          </svg>
        )}
        {hovered && hover != null && (
          <Tooltip x={PAD.left + slot * hover + slot / 2} width={width}>
            <p className="mb-1 font-semibold text-fg">{label(hovered)}</p>
            {KINDS.map((k) => (
              <TooltipRow key={k.key} color={k.color} label={k.label} value={formatWatchTime(hovered[k.key])} />
            ))}
            <p className="mt-1 flex border-t border-line pt-1">
              <span className="text-muted">Gesamt</span>
              <span className="ml-auto font-semibold text-fg tabular-nums">{formatWatchTime(total(hovered))}</span>
            </p>
          </Tooltip>
        )}
      </div>
    </div>
  )
}

/** A bar path with only the top corners rounded (anchored to the baseline). */
function roundedTop(x: number, y: number, w: number, h: number, r: number): string {
  if (r <= 0) return `M${x},${y}h${w}v${h}h${-w}Z`
  return `M${x},${y + h}V${y + r}Q${x},${y} ${x + r},${y}H${x + w - r}Q${x + w},${y} ${x + w},${y + r}V${y + h}Z`
}
