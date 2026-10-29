import clsx from 'clsx'

/** Wordmark; collapses to the "f" monogram in the icon rail. */
export function Logo({ compact = false }: { compact?: boolean }) {
  return (
    <span className="flex items-center gap-2 select-none" aria-label="Flixnet">
      <svg viewBox="0 0 64 64" className="size-8 shrink-0" aria-hidden="true">
        <rect width="64" height="64" rx="14" className="fill-surface-3" />
        <path d="M22 16h24v8H31v7h13v8H31v13h-9z" className="fill-fg" />
        <circle cx="46" cy="46" r="6" className="fill-accent" />
      </svg>
      <span className={clsx('text-lg font-extrabold tracking-tight', compact && 'sr-only')}>
        flixnet
      </span>
    </span>
  )
}
