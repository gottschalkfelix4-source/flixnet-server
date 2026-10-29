import { decode } from 'blurhash'
import clsx from 'clsx'
import { useMemo, useState } from 'react'

const cache = new Map<string, string>()

/** Decodes a blurhash into a tiny data URL (cached; 32x32 is plenty when scaled up). */
function blurhashUrl(hash: string): string | null {
  const cached = cache.get(hash)
  if (cached) return cached
  try {
    const pixels = decode(hash, 32, 32)
    const canvas = document.createElement('canvas')
    canvas.width = 32
    canvas.height = 32
    const ctx = canvas.getContext('2d')
    if (!ctx) return null
    const image = ctx.createImageData(32, 32)
    image.data.set(pixels)
    ctx.putImageData(image, 0, 0)
    const url = canvas.toDataURL()
    cache.set(hash, url)
    return url
  } catch {
    return null
  }
}

interface BlurImageProps {
  src?: string | null
  blurhash?: string | null
  alt: string
  className?: string
  /** Load eagerly (hero, above the fold). */
  eager?: boolean
}

/** Image that shows its blurhash (or a neutral surface) until the real image has loaded. */
export function BlurImage({ src, blurhash, alt, className, eager = false }: BlurImageProps) {
  // Tracks which src finished loading, so a new src fades in again.
  const [loadedSrc, setLoadedSrc] = useState<string | null>(null)
  const loaded = !!src && loadedSrc === src
  const placeholder = useMemo(() => (blurhash ? blurhashUrl(blurhash) : null), [blurhash])
  // Callers may position the box themselves (e.g. `absolute inset-0`); `relative` would override it.
  const positioned = /(^|\s)(absolute|fixed|sticky)(\s|$)/.test(className ?? '')

  return (
    <div className={clsx(!positioned && 'relative', 'overflow-hidden bg-surface-2', className)}>
      {placeholder && !loaded && (
        <img src={placeholder} alt="" aria-hidden="true" className="absolute inset-0 size-full object-cover" />
      )}
      {src && (
        <img
          src={src}
          alt={alt}
          loading={eager ? 'eager' : 'lazy'}
          decoding="async"
          onLoad={() => setLoadedSrc(src)}
          className={clsx(
            'absolute inset-0 size-full object-cover transition-opacity duration-500',
            loaded ? 'opacity-100' : 'opacity-0',
          )}
        />
      )}
    </div>
  )
}
