import type { ReactNode } from 'react'

export function Legend({ items }: { items: { label: string; color: string }[] }) {
  return (
    <ul className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted" aria-label="Legende">
      {items.map((i) => (
        <li key={i.label} className="flex items-center gap-1.5">
          <span className="size-2.5 rounded-full" style={{ background: i.color }} aria-hidden="true" />
          {i.label}
        </li>
      ))}
    </ul>
  )
}

/** Hover card placed next to the pointer, kept inside the chart. */
export function Tooltip({ x, width, children }: { x: number; width: number; children: ReactNode }) {
  const left = Math.min(Math.max(x + 12, 0), Math.max(0, width - 200))
  return (
    <div
      role="tooltip"
      className="pointer-events-none absolute top-2 z-10 w-48 rounded-lg border border-line bg-surface-3/95 px-3 py-2 text-xs shadow-xl backdrop-blur"
      style={{ left }}
    >
      {children}
    </div>
  )
}

export function TooltipRow({ color, label, value }: { color: string; label: string; value: string }) {
  return (
    <p className="flex items-center gap-2 py-0.5">
      <span className="size-2 rounded-full" style={{ background: color }} aria-hidden="true" />
      <span className="text-muted">{label}</span>
      <span className="ml-auto font-semibold text-fg tabular-nums">{value}</span>
    </p>
  )
}
