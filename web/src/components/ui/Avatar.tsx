import clsx from 'clsx'

// Warm-to-cool gradients; picked by user id so a profile keeps its color.
const GRADIENTS = [
  'from-[#ff6b3d] to-[#c2410c]',
  'from-[#f59e0b] to-[#b45309]',
  'from-[#10b981] to-[#047857]',
  'from-[#06b6d4] to-[#0e7490]',
  'from-[#6366f1] to-[#4338ca]',
  'from-[#d946ef] to-[#a21caf]',
  'from-[#f43f5e] to-[#be123c]',
]

export function Avatar({
  id,
  name,
  size = 'md',
  className,
}: {
  id: number
  name: string
  size?: 'xs' | 'sm' | 'md' | 'xl'
  className?: string
}) {
  return (
    <span
      aria-hidden="true"
      className={clsx(
        'flex shrink-0 items-center justify-center bg-gradient-to-br font-extrabold text-white/95',
        GRADIENTS[Math.abs(id) % GRADIENTS.length],
        size === 'xs' && 'size-4 rounded-full text-[9px]',
        size === 'sm' && 'size-9 rounded-full text-sm',
        size === 'md' && 'size-11 rounded-full text-base',
        size === 'xl' && 'size-28 rounded-2xl text-5xl md:size-32',
        className,
      )}
    >
      {name.charAt(0).toUpperCase()}
    </span>
  )
}
