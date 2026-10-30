import { Link } from '@tanstack/react-router'
import clsx from 'clsx'
import { Check, Film, Play } from 'lucide-react'
import { useState } from 'react'
import type { ItemCard } from '@/api/queries/items'
import { episodeLabel } from '@/lib/media'
import { BlurImage } from './BlurImage'
import { CardMenu } from './CardMenu'

interface PosterCardProps {
  item: ItemCard
  /** Wide cards (16:9) use the backdrop/still instead of the poster. */
  wide?: boolean
  className?: string
}

/** What a card says under its title, as in Plex: year, "3 Staffeln", "S1 · F2 · Titel". */
function caption(item: ItemCard): string {
  if (item.kind === 'episode' && item.series_title) return [episodeLabel(item), item.title].filter(Boolean).join(' · ')
  if (item.kind === 'series' && item.child_count > 0) return `${item.child_count} ${item.child_count === 1 ? 'Staffel' : 'Staffeln'}`
  if (item.kind === 'season' && item.child_count > 0) return `${item.child_count} ${item.child_count === 1 ? 'Folge' : 'Folgen'}`
  return episodeLabel(item) ?? (item.year != null ? String(item.year) : ' ')
}

/**
 * Poster (2:3) or landscape (16:9) card like Plex': watched check or unwatched-episode
 * count in the top corner, progress along the bottom edge; on hover a thin accent
 * outline, a play button in the middle and the "⋮" actions.
 */
export function PosterCard({ item, wide = false, className }: PosterCardProps) {
  const [menuOpen, setMenuOpen] = useState(false)
  // Episodes read "Serie" / "S1 · F2 · Folgentitel" so rows mixing series stay clear.
  const heading = item.kind === 'episode' && item.series_title ? item.series_title : item.title
  const image = wide ? (item.backdrop_url ?? item.poster_url) : item.poster_url
  const progress = item.position_ms > 0 && item.runtime_ms ? Math.min(1, item.position_ms / item.runtime_ms) : null
  const playable = item.kind === 'movie' || item.kind === 'episode'
  const unplayed = !item.played && (item.kind === 'series' || item.kind === 'season') ? item.unplayed_count : 0
  const aspect = wide ? 'aspect-video' : 'aspect-[2/3]'

  return (
    // The overlay controls sit next to (not inside) the link: no nested interactive elements.
    <div className={clsx('group/card relative min-w-0', className)}>
      <Link
        to="/item/$id"
        params={{ id: String(item.id) }}
        className="group block min-w-0 focus-visible:outline-none"
        data-testid="poster-card"
      >
        <div
          className={clsx(
            'relative overflow-hidden rounded-md bg-surface-2 ring-accent transition-shadow group-hover/card:ring-1 group-focus-visible:ring-2',
            menuOpen && 'ring-1',
            aspect,
          )}
        >
          <BlurImage src={image} blurhash={wide ? null : item.poster_blurhash} alt={item.title} className="absolute inset-0" />
          {!image && (
            <div className="absolute inset-0 flex flex-col items-center justify-center gap-2 p-3 text-center">
              <Film className="size-7 text-subtle" aria-hidden="true" />
              <span className="line-clamp-3 text-sm font-semibold text-muted">{item.title}</span>
            </div>
          )}
          {/* Dims the artwork under the hover controls. */}
          <span
            className={clsx(
              'absolute inset-0 bg-black/45 opacity-0 transition-opacity group-hover/card:opacity-100',
              menuOpen && 'opacity-100',
            )}
            aria-hidden="true"
          />
          {item.played ? (
            <span className="absolute top-0 right-0 flex size-7 items-center justify-center rounded-bl-md bg-black/75" title="Gesehen">
              <Check className="size-4 text-white" strokeWidth={3} aria-label="Gesehen" />
            </span>
          ) : unplayed > 0 ? (
            <span
              className="absolute top-0 right-0 flex h-7 min-w-7 items-center justify-center rounded-bl-md bg-black/80 px-1.5 text-xs font-bold text-white tabular-nums"
              title={`${unplayed} ungesehen`}
              data-testid="unplayed-count"
            >
              {unplayed}
            </span>
          ) : null}
          {/* Also on watched items being watched again. */}
          {progress !== null && (
            <span className="absolute inset-x-0 bottom-0 h-1 bg-black/60" aria-hidden="true">
              <span className="block h-full bg-accent" style={{ width: `${Math.round(progress * 100)}%` }} />
            </span>
          )}
        </div>
        <div className="mt-2 px-0.5">
          <p className="truncate text-sm font-semibold text-fg">{heading}</p>
          <p className="truncate text-xs text-subtle">{caption(item)}</p>
        </div>
      </Link>

      {/* Hover controls over the artwork (same box as the image). */}
      <div
        className={clsx(
          'pointer-events-none absolute inset-x-0 top-0 opacity-0 transition-opacity group-focus-within/card:opacity-100 group-hover/card:opacity-100',
          menuOpen && 'opacity-100',
          aspect,
        )}
      >
        {playable && (
          <Link
            to="/play/$id"
            params={{ id: String(item.id) }}
            aria-label={`Wiedergabe starten: ${item.title}`}
            title="Abspielen"
            className="pointer-events-auto absolute top-1/2 left-1/2 flex size-12 -translate-x-1/2 -translate-y-1/2 items-center justify-center rounded-full border-2 border-white/90 bg-black/30 text-white transition-colors hover:border-accent hover:bg-accent hover:text-accent-fg"
          >
            <Play className="ml-0.5 size-5 fill-current" aria-hidden="true" />
          </Link>
        )}
        <div className="pointer-events-auto absolute right-1 bottom-1">
          <CardMenu card={item} onOpenChange={setMenuOpen} />
        </div>
      </div>
    </div>
  )
}

export function PosterSkeleton({ wide = false }: { wide?: boolean }) {
  return (
    <div aria-hidden="true">
      <div className={clsx('animate-pulse rounded-md bg-surface-2', wide ? 'aspect-video' : 'aspect-[2/3]')} />
      <div className="mt-2 h-3.5 w-3/4 animate-pulse rounded bg-surface-2" />
    </div>
  )
}
