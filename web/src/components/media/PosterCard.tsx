import { Link } from '@tanstack/react-router'
import clsx from 'clsx'
import { Check, Film } from 'lucide-react'
import type { ItemCard } from '@/api/queries/items'
import { episodeLabel } from '@/lib/media'
import { BlurImage } from './BlurImage'

interface PosterCardProps {
  item: ItemCard
  /** Wide cards (16:9) use the backdrop/still instead of the poster. */
  wide?: boolean
  className?: string
}

/** Poster (2:3) or landscape (16:9) card with title, progress and watched state. */
export function PosterCard({ item, wide = false, className }: PosterCardProps) {
  const label = episodeLabel(item)
  // Episodes read "Serie" / "S1 · F2 · Folgentitel" so rows mixing series stay clear.
  const episodeOfSeries = item.kind === 'episode' && item.series_title
  const heading = episodeOfSeries ? item.series_title : item.title
  const caption = episodeOfSeries ? [label, item.title].filter(Boolean).join(' · ') : (label ?? item.year ?? ' ')
  const image = wide ? (item.backdrop_url ?? item.poster_url) : item.poster_url
  const progress =
    item.position_ms > 0 && item.runtime_ms ? Math.min(1, item.position_ms / item.runtime_ms) : null

  return (
    <Link
      to="/item/$id"
      params={{ id: String(item.id) }}
      className={clsx('group block min-w-0 focus-visible:outline-none', className)}
      data-testid="poster-card"
    >
      <div
        className={clsx(
          'relative overflow-hidden rounded-[var(--radius-card)] shadow-[var(--shadow-card)] ring-accent transition duration-200 group-hover:-translate-y-0.5 group-hover:ring-2 group-focus-visible:ring-2',
          wide ? 'aspect-video' : 'aspect-[2/3]',
        )}
      >
        <BlurImage src={image} blurhash={wide ? null : item.poster_blurhash} alt={item.title} className="absolute inset-0" />
        {!image && (
          <div className="absolute inset-0 flex flex-col items-center justify-center gap-2 p-3 text-center">
            <Film className="size-7 text-subtle" aria-hidden="true" />
            <span className="line-clamp-3 text-sm font-semibold text-muted">{item.title}</span>
          </div>
        )}
        {item.played && (
          <span
            className="absolute top-2 right-2 flex size-6 items-center justify-center rounded-full bg-accent text-accent-fg shadow"
            title="Gesehen"
          >
            <Check className="size-4" strokeWidth={3} aria-label="Gesehen" />
          </span>
        )}
        {/* Also on watched items being watched again. */}
        {progress !== null && (
          <div className="absolute inset-x-2 bottom-2 h-1 overflow-hidden rounded-full bg-black/60">
            <div className="h-full bg-accent" style={{ width: `${Math.round(progress * 100)}%` }} />
          </div>
        )}
      </div>
      <div className="mt-2 px-0.5">
        <p className="truncate text-sm font-semibold text-fg group-hover:text-white">{heading}</p>
        <p className="truncate text-xs text-subtle">
          {caption}
          {item.kind === 'series' && item.child_count > 0 &&
            ` · ${item.child_count} ${item.child_count === 1 ? 'Staffel' : 'Staffeln'}`}
        </p>
      </div>
    </Link>
  )
}

export function PosterSkeleton({ wide = false }: { wide?: boolean }) {
  return (
    <div aria-hidden="true">
      <div className={clsx('animate-pulse rounded-[var(--radius-card)] bg-surface-2', wide ? 'aspect-video' : 'aspect-[2/3]')} />
      <div className="mt-2 h-3.5 w-3/4 animate-pulse rounded bg-surface-2" />
    </div>
  )
}
