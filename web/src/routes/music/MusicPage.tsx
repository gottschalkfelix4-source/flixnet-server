import { Link, useNavigate, useSearch } from '@tanstack/react-router'
import clsx from 'clsx'
import { Heart, Music, Play, Shuffle } from 'lucide-react'
import { useGenres } from '@/api/queries/items'
import { useItemPages, useMusicLibraries, useTracks } from '@/api/queries/music'
import { AlbumCard, ArtistCard, CardGrid } from '@/components/music/MusicCards'
import { TrackList } from '@/components/music/TrackList'
import { Button } from '@/components/ui/Button'
import { EmptyState } from '@/components/ui/EmptyState'
import { PageHeader } from '@/components/ui/PageHeader'
import { musicPlayer } from '@/lib/music/player'
import { MUSIC_TABS, type MusicSearch, type MusicTab } from './search'

const TAB_LABELS: Record<MusicTab, string> = {
  albums: 'Alben',
  artists: 'Künstler',
  genres: 'Genres',
  tracks: 'Titel',
}

/** Music: albums (default), artists, genres and tracks of the music libraries. */
export function MusicPage() {
  const search = useSearch({ from: '/app/music' })
  const navigate = useNavigate({ from: '/music' })
  const libraries = useMusicLibraries()
  const tab = search.tab ?? 'albums'

  if (libraries.data && libraries.data.length === 0) {
    return (
      <div>
        <PageHeader title="Musik" />
        <EmptyState icon={Music} title="Noch keine Musik">
          Lege in den Einstellungen eine Bibliothek vom Typ „Musik“ an.
        </EmptyState>
      </div>
    )
  }
  const libraryId = search.library ?? (libraries.data?.length === 1 ? libraries.data[0]?.id : undefined)

  return (
    <div className="pb-16">
      <PageHeader title="Musik">
        {libraries.data && libraries.data.length > 1 && (
          <select
            aria-label="Bibliothek"
            value={search.library ?? ''}
            onChange={(e) =>
              void navigate({ search: { ...search, library: e.target.value ? Number(e.target.value) : undefined } })
            }
            className="h-10 rounded-lg border border-line bg-surface-2 px-3 text-sm"
          >
            <option value="">Alle Musikbibliotheken</option>
            {libraries.data.map((l) => (
              <option key={l.id} value={l.id}>
                {l.name}
              </option>
            ))}
          </select>
        )}
      </PageHeader>
      <nav className="mb-6 flex gap-2 px-6 md:px-10" aria-label="Musikbereiche">
        {MUSIC_TABS.map((t) => (
          <Link
            key={t}
            to="/music"
            search={{ ...search, tab: t === 'albums' ? undefined : t, genre: undefined }}
            className={clsx(
              'rounded-full px-4 py-1.5 text-sm font-semibold transition-colors',
              t === tab ? 'bg-fg text-canvas' : 'bg-surface-2 text-muted hover:text-fg',
            )}
            aria-current={t === tab ? 'page' : undefined}
          >
            {TAB_LABELS[t]}
          </Link>
        ))}
      </nav>
      <div className="px-6 md:px-10">
        {tab === 'albums' && <Albums libraryId={libraryId} genre={search.genre} sort={search.sort ?? 'title'} />}
        {tab === 'artists' && <Artists libraryId={libraryId} />}
        {tab === 'genres' && <Genres libraryId={libraryId} />}
        {tab === 'tracks' && <Tracks libraryId={libraryId} />}
      </div>
    </div>
  )
}

function LoadMore({ query }: { query: { hasNextPage: boolean; isFetchingNextPage: boolean; fetchNextPage: () => unknown } }) {
  if (!query.hasNextPage) return null
  return (
    <div className="mt-8 flex justify-center">
      <Button variant="ghost" loading={query.isFetchingNextPage} onClick={() => void query.fetchNextPage()}>
        Mehr laden
      </Button>
    </div>
  )
}

function Albums({ libraryId, genre, sort }: { libraryId?: number; genre?: string; sort: string }) {
  const navigate = useNavigate({ from: '/music' })
  const pages = useItemPages({
    kind: 'album',
    library_id: libraryId,
    genre,
    sort: sort as 'title',
    order: sort === 'title' ? 'asc' : 'desc',
  })
  const albums = pages.data?.pages.flatMap((p) => p.items) ?? []
  const total = pages.data?.pages[0]?.total ?? 0
  return (
    <section aria-label="Alben">
      <div className="mb-4 flex flex-wrap items-center gap-3">
        {genre && (
          <span className="rounded-full bg-accent/15 px-3 py-1 text-sm font-semibold text-accent">
            {genre}
            <Link to="/music" search={(s: MusicSearch) => ({ ...s, genre: undefined })} className="ml-2" aria-label="Genre-Filter entfernen">
              ×
            </Link>
          </span>
        )}
        <span className="text-sm text-subtle">{total} Alben</span>
        <select
          aria-label="Sortierung"
          value={sort}
          onChange={(e) => void navigate({ search: (s: MusicSearch) => ({ ...s, sort: e.target.value as 'title' }) })}
          className="ml-auto h-9 rounded-lg border border-line bg-surface-2 px-3 text-sm"
        >
          <option value="title">Titel</option>
          <option value="date_added">Neu hinzugefügt</option>
          <option value="year">Erscheinungsjahr</option>
        </select>
      </div>
      {pages.data && albums.length === 0 && <p className="text-sm text-muted">Keine Alben gefunden.</p>}
      <CardGrid>
        {albums.map((album) => (
          <AlbumCard key={album.id} album={album} />
        ))}
      </CardGrid>
      <LoadMore query={pages} />
    </section>
  )
}

function Artists({ libraryId }: { libraryId?: number }) {
  const pages = useItemPages({ kind: 'artist', library_id: libraryId, sort: 'title' })
  const artists = pages.data?.pages.flatMap((p) => p.items) ?? []
  return (
    <section aria-label="Künstler">
      {pages.data && artists.length === 0 && <p className="text-sm text-muted">Keine Künstler gefunden.</p>}
      <CardGrid round>
        {artists.map((artist) => (
          <ArtistCard key={artist.id} artist={artist} />
        ))}
      </CardGrid>
      <LoadMore query={pages} />
    </section>
  )
}

function Genres({ libraryId }: { libraryId?: number }) {
  const libraries = useMusicLibraries()
  // Genres of the music libraries only (films share the genre table).
  const genres = useGenres(libraryId ?? libraries.data?.[0]?.id)
  return (
    <section aria-label="Genres" className="flex flex-wrap gap-3">
      {genres.data?.length === 0 && <p className="text-sm text-muted">Keine Genres in den Tags.</p>}
      {genres.data?.map((g) => (
        <Link
          key={g.name}
          to="/music"
          search={{ tab: undefined, genre: g.name, library: libraryId }}
          className="rounded-[var(--radius-card)] bg-surface-2 px-5 py-4 text-sm font-bold transition-colors hover:bg-surface-3"
        >
          {g.name}
          <span className="ml-2 text-xs font-normal text-subtle">{g.count}</span>
        </Link>
      ))}
    </section>
  )
}

function Tracks({ libraryId }: { libraryId?: number }) {
  const navigate = useNavigate({ from: '/music' })
  const search = useSearch({ from: '/app/music' })
  const onlyFavorites = search.favorites === true
  const tracks = useTracks({ library_id: libraryId, favorite: onlyFavorites || undefined, sort: 'title', limit: 500 })
  const items = tracks.data?.items ?? []
  return (
    <section aria-label="Titel">
      <div className="mb-4 flex flex-wrap items-center gap-3">
        <Button variant="accent" className="h-10" disabled={items.length === 0} onClick={() => musicPlayer.playTracks(items, 0)}>
          <Play className="size-4" fill="currentColor" aria-hidden="true" />
          Alle abspielen
        </Button>
        <Button
          variant="ghost"
          className="h-10"
          disabled={items.length === 0}
          onClick={() => musicPlayer.playTracks(items, 0, { shuffle: true })}
        >
          <Shuffle className="size-4" aria-hidden="true" />
          Zufällig
        </Button>
        <button
          type="button"
          aria-pressed={onlyFavorites}
          onClick={() => void navigate({ search: (s: MusicSearch) => ({ ...s, favorites: onlyFavorites ? undefined : true }) })}
          className={clsx(
            'ml-auto inline-flex items-center gap-2 rounded-full px-4 py-1.5 text-sm font-semibold',
            onlyFavorites ? 'bg-accent text-accent-fg' : 'bg-surface-2 text-muted hover:text-fg',
          )}
        >
          <Heart className="size-4" aria-hidden="true" />
          Favoriten
        </button>
        <span className="text-sm text-subtle">{tracks.data?.total ?? 0} Titel</span>
      </div>
      <TrackList tracks={items} showAlbum />
    </section>
  )
}
