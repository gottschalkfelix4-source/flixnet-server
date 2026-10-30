import clsx from 'clsx'
import { Check, ChevronDown } from 'lucide-react'
import { type ReactNode, useCallback, useId, useRef, useState } from 'react'
import { useDismiss } from '@/components/layout/useDismiss'

export interface FilterOption<T extends string> {
  value: T
  label: string
  /** Shown at the end of the row, e.g. the sort direction of the chosen sort. */
  hint?: ReactNode
}

/**
 * A text button with "▾" that opens a list of choices, like the filter bar of a
 * Plex library ("Alle ▾", "Nach Titel ▾").
 */
export function FilterMenu<T extends string>({
  label,
  name,
  value,
  options,
  onSelect,
}: {
  /** What the button shows (usually the current choice). */
  label: string
  /** Accessible name of the button and the list, e.g. "Genre". */
  name: string
  value: T
  options: FilterOption<T>[]
  onSelect: (value: T) => void
}) {
  const [open, setOpen] = useState(false)
  const close = useCallback(() => setOpen(false), [])
  const root = useRef<HTMLDivElement>(null)
  useDismiss(root, open, close)
  const listId = useId()

  return (
    <div ref={root} className="relative">
      <button
        type="button"
        aria-label={`${name}: ${label}`}
        aria-expanded={open}
        aria-controls={listId}
        onClick={() => setOpen((o) => !o)}
        className={clsx(
          'flex h-9 items-center gap-1 rounded-md px-2 text-[15px] font-semibold transition-colors hover:bg-surface-2',
          open ? 'text-fg' : 'text-fg/90',
        )}
      >
        {label}
        <ChevronDown className="size-4 text-muted" aria-hidden="true" />
      </button>
      {open && (
        <ul
          id={listId}
          role="menu"
          aria-label={name}
          className="absolute top-10 left-0 z-30 max-h-80 min-w-48 overflow-y-auto rounded-lg border border-line bg-surface-2 py-1 shadow-2xl"
        >
          {options.map((o) => (
            <li key={o.value} role="none">
              <button
                type="button"
                role="menuitemradio"
                aria-checked={o.value === value}
                onClick={() => {
                  onSelect(o.value)
                  setOpen(false)
                }}
                className={clsx(
                  'flex w-full items-center gap-2 px-4 py-2 text-left text-sm transition-colors hover:bg-surface-3',
                  o.value === value ? 'font-semibold text-accent' : 'text-muted hover:text-fg',
                )}
              >
                <Check className={clsx('size-4 shrink-0', o.value !== value && 'invisible')} aria-hidden="true" />
                <span className="flex-1">{o.label}</span>
                {o.hint}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}
