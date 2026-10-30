import { useRef, useState } from 'react'
import type { Schemas } from '@/api/client'
import { formatRate } from '@/lib/bytes'
import { Legend, Tooltip, TooltipRow } from './chart'
import { SERIES, niceTicks, useWidth } from './scale'

type Point = Schemas['BandwidthPointDto']

const HEIGHT = 200
const PAD = { top: 12, right: 12, bottom: 24, left: 64 }

const time = new Intl.DateTimeFormat('de', { hour: '2-digit', minute: '2-digit' })
const timeSeconds = new Intl.DateTimeFormat('de', { hour: '2-digit', minute: '2-digit', second: '2-digit' })

/** Local and remote media bandwidth over time: two 2px lines on a recessive grid. */
export function BandwidthChart({ points, slotSeconds }: { points: Point[]; slotSeconds: number }) {
  const box = useRef<HTMLDivElement>(null)
  const width = useWidth(box)
  const [hover, setHover] = useState<number | null>(null)
  const plotW = Math.max(0, width - PAD.left - PAD.right)
  const plotH = HEIGHT - PAD.top - PAD.bottom
  const max = Math.max(1, ...points.map((p) => Math.max(p.local_bytes_per_second, p.remote_bytes_per_second)))
  const ticks = niceTicks(max)
  const top = ticks[ticks.length - 1]!
  const x = (i: number) => PAD.left + (points.length > 1 ? (i / (points.length - 1)) * plotW : plotW)
  const y = (v: number) => PAD.top + plotH - (v / top) * plotH
  const line = (key: 'local_bytes_per_second' | 'remote_bytes_per_second') =>
    points.map((p, i) => `${i === 0 ? 'M' : 'L'}${x(i).toFixed(1)},${y(p[key]).toFixed(1)}`).join(' ')
  const format = slotSeconds < 60 ? timeSeconds : time
  // About one label per 80px, so they never overlap on narrow screens.
  const labelEvery = Math.max(1, Math.ceil(points.length / Math.max(2, Math.floor(plotW / 80))))
  const hovered = hover != null ? points[hover] : undefined

  return (
    <div className="flex flex-col gap-3">
      <Legend
        items={[
          { label: 'Lokal', color: SERIES.local },
          { label: 'Remote', color: SERIES.remote },
        ]}
      />
      <div ref={box} className="relative" data-testid="bandwidth-chart">
        {width > 0 && (
          <svg
            width={width}
            height={HEIGHT}
            role="img"
            aria-label="Bandbreite lokal und remote im Zeitverlauf"
            onPointerMove={(e) => {
              const rect = e.currentTarget.getBoundingClientRect()
              const i = Math.round(((e.clientX - rect.left - PAD.left) / Math.max(1, plotW)) * (points.length - 1))
              setHover(i >= 0 && i < points.length ? i : null)
            }}
            onPointerLeave={() => setHover(null)}
          >
            {ticks.map((t) => (
              <g key={t}>
                <line x1={PAD.left} x2={PAD.left + plotW} y1={y(t)} y2={y(t)} stroke="var(--color-line)" strokeWidth={1} />
                <text x={PAD.left - 8} y={y(t) + 4} textAnchor="end" className="fill-subtle text-[11px] tabular-nums">
                  {formatRate(t)}
                </text>
              </g>
            ))}
            {points.map((p, i) =>
              (i % labelEvery === 0 && points.length - 1 - i >= labelEvery / 2) || i === points.length - 1 ? (
                <text key={p.at} x={x(i)} y={HEIGHT - 6} textAnchor="middle" className="fill-subtle text-[11px] tabular-nums">
                  {i === points.length - 1 ? 'jetzt' : format.format(p.at)}
                </text>
              ) : null,
            )}
            <path d={line('remote_bytes_per_second')} fill="none" stroke={SERIES.remote} strokeWidth={2} strokeLinejoin="round" />
            <path d={line('local_bytes_per_second')} fill="none" stroke={SERIES.local} strokeWidth={2} strokeLinejoin="round" />
            {hovered && hover != null && (
              <g>
                <line x1={x(hover)} x2={x(hover)} y1={PAD.top} y2={PAD.top + plotH} stroke="var(--color-muted)" strokeWidth={1} />
                {(['local_bytes_per_second', 'remote_bytes_per_second'] as const).map((key) => (
                  <circle
                    key={key}
                    cx={x(hover)}
                    cy={y(hovered[key])}
                    r={4}
                    fill={key === 'local_bytes_per_second' ? SERIES.local : SERIES.remote}
                    stroke="var(--color-surface)"
                    strokeWidth={2}
                  />
                ))}
              </g>
            )}
          </svg>
        )}
        {hovered && hover != null && (
          <Tooltip x={x(hover)} width={width}>
            <p className="mb-1 font-semibold text-fg">{format.format(hovered.at)}</p>
            <TooltipRow color={SERIES.local} label="Lokal" value={formatRate(hovered.local_bytes_per_second)} />
            <TooltipRow color={SERIES.remote} label="Remote" value={formatRate(hovered.remote_bytes_per_second)} />
          </Tooltip>
        )}
      </div>
    </div>
  )
}
