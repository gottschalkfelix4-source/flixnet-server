import { Link } from '@tanstack/react-router'
import clsx from 'clsx'
import type { ReactNode } from 'react'
import { Disc3, MicVocal } from 'lucide-react'
import type { ItemCard } from '@/api/queries/items'
import { BlurImage } from '@/components/media/BlurImage'

/** Square album cover with title, artist and year. */
export function AlbumCard({ album, showArtist = true }: { album: ItemCard; showArtist?: boolean }) {
  return (
    <Link
      to="/music/album/$id"
      params={{ id: String(album.id) }}
      className="group block min-w-0 focus-visible:outline-none"
      data-testid="album-card"
    >
      <div className="relative aspect-square overflow-hidden rounded-[var(--radius-card)] shadow-[var(--shadow-card)] ring-accent transition duration-200 group-hover:-translate-y-0.5 group-hover:ring-2 group-focus-visible:ring-2">
        <BlurImage src={album.poster_url} blurhash={album.poster_blurhash} alt={album.title} className="absolute inset-0" />
        {!album.poster_url && (
          <div className="absolute inset-0 flex items-center justify-center">
            <Disc3 className="size-10 text-subtle" aria-hidden="true" />
          </div>
        )}
      </div>
      <div className="mt-2 px-0.5">
        <p className="truncate text-sm font-semibold text-fg group-hover:text-white">{album.title}</p>
        <p className="truncate text-xs text-subtle">
          {[showArtist ? album.series_title : null, album.year].filter(Boolean).join(' · ') || ' '}
        </p>
      </div>
    </Link>
  )
}

/** Round artist picture with name and album count. */
export function ArtistCard({ artist }: { artist: ItemCard }) {
  return (
    <Link
      to="/music/artist/$id"
      params={{ id: String(artist.id) }}
      className="group block min-w-0 text-center focus-visible:outline-none"
      data-testid="artist-card"
    >
      <div className="relative mx-auto aspect-square overflow-hidden rounded-full shadow-[var(--shadow-card)] ring-accent transition group-hover:ring-2 group-focus-visible:ring-2">
        <BlurImage src={artist.poster_url} blurhash={artist.poster_blurhash} alt={artist.title} className="absolute inset-0" />
        {!artist.poster_url && (
          <div className="absolute inset-0 flex items-center justify-center">
            <MicVocal className="size-10 text-subtle" aria-hidden="true" />
          </div>
        )}
      </div>
      <p className="mt-2 truncate text-sm font-semibold group-hover:text-white">{artist.title}</p>
      <p className="truncate text-xs text-subtle">
        {artist.child_count} {artist.child_count === 1 ? 'Album' : 'Alben'}
      </p>
    </Link>
  )
}

export function CardGrid({ children, round = false }: { children: ReactNode; round?: boolean }) {
  return (
    <div
      className={clsx(
        'grid gap-x-4 gap-y-6',
        round
          ? 'grid-cols-[repeat(auto-fill,minmax(130px,1fr))]'
          : 'grid-cols-[repeat(auto-fill,minmax(150px,1fr))]',
      )}
    >
      {children}
    </div>
  )
}
