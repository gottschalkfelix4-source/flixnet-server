import clsx from 'clsx'

/**
 * The Flixnet mark as a loading animation: the "F" draws itself and fills in, the
 * accent dot pulses and sends out a ring. Without motion it is the plain logo.
 */
export function LogoLoader({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 64 64" className={clsx('overflow-visible', className)} aria-hidden="true">
      <rect width="64" height="64" rx="14" className="fill-white/10" />
      <path
        d="M22 16h24v8H31v7h13v8H31v13h-9z"
        pathLength={100}
        className="fill-white stroke-white motion-safe:animate-logo-draw"
        strokeWidth={1.5}
        strokeLinejoin="round"
        strokeDasharray={100}
      />
      <circle cx="46" cy="46" r="6" className="origin-center fill-accent [transform-box:fill-box] motion-safe:animate-logo-ping" />
      <circle cx="46" cy="46" r="6" className="origin-center fill-accent [transform-box:fill-box] motion-safe:animate-logo-dot" />
    </svg>
  )
}
