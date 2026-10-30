import { Link, useParams } from '@tanstack/react-router'
import { Disc3, ListPlus, Play, Shuffle } from 'lucide-react'
import { useAlbum } from '@/api/queries/music'
import { BlurImage } from '@/components/media/BlurImage'
import { TrackList } from '@/components/music/TrackList'
import { Button } from '@/components/ui/Button'
import { EmptyState } from '@/components/ui/EmptyState'
import { formatRuntime } from '@/lib/media'
import { musicPlayer } from '@/lib/music/player'

/** An album: cover, facts, tracks grouped by disc. */
export function AlbumPage() {
  const { id } = useParams({ from: '/app/music/album/$id' })
  const album = useAlbum(Number(id))

  if (album.isError) {
    return (
      <div className="pt-10">
        <EmptyState icon={Disc3} title="Album nicht gefunden" />
      </div>
    )
  }
  if (!album.data) return null
  const a = album.data
  const tracks = a.discs.flatMap((d) => d.tracks)
  const multiDisc = a.discs.length > 1
  const facts = [
    a.year,
    `${a.track_count} ${a.track_count === 1 ? 'Titel' : 'Titel'}`,
    formatRuntime(a.duration_ms),
    ...a.genres,
  ].filter(Boolean)

  return (
    <div className="px-6 pt-8 pb-16 md:px-10">
      <header className="mb-8 flex flex-col gap-6 md:flex-row md:items-end">
        <div className="relative aspect-square w-48 shrink-0 overflow-hidden rounded-[var(--radius-card)] shadow-[var(--shadow-card)] md:w-60">
          <BlurImage src={a.cover_url ?? a.poster_url} blurhash={a.poster_blurhash} alt={a.title} className="absolute inset-0" eager />
          {!a.cover_url && !a.poster_url && <Disc3 className="absolute inset-0 m-auto size-14 text-subtle" aria-hidden="true" />}
        </div>
        <div className="min-w-0">
          <p className="text-xs font-bold tracking-widest text-subtle uppercase">Album</p>
          <h1 className="mt-1 text-3xl font-extrabold tracking-tight md:text-5xl">{a.title}</h1>
          {a.artist_id != null && (
            <Link
              to="/music/artist/$id"
              params={{ id: String(a.artist_id) }}
              className="mt-2 inline-block text-lg font-semibold text-muted hover:text-fg hover:underline"
            >
              {a.series_title}
            </Link>
          )}
          <p className="mt-2 text-sm text-subtle">{facts.join(' · ')}</p>
          <div className="mt-5 flex flex-wrap gap-3">
            <Button variant="accent" onClick={() => musicPlayer.playTracks(tracks, 0, { shuffle: false })} disabled={tracks.length === 0}>
              <Play className="size-4" fill="currentColor" aria-hidden="true" />
              Abspielen
            </Button>
            <Button variant="ghost" onClick={() => musicPlayer.playTracks(tracks, 0, { shuffle: true })} disabled={tracks.length === 0}>
              <Shuffle className="size-4" aria-hidden="true" />
              Zufällig
            </Button>
            <Button variant="ghost" onClick={() => musicPlayer.enqueue(tracks)} disabled={tracks.length === 0}>
              <ListPlus className="size-4" aria-hidden="true" />
              Zur Warteschlange
            </Button>
          </div>
        </div>
      </header>
      {a.discs.map((disc) => (
        <section key={disc.number} className="mb-6" aria-label={multiDisc ? `CD ${disc.number}` : 'Titel'}>
          {multiDisc && <h2 className="mb-2 px-2 text-sm font-bold text-muted">CD {disc.number}</h2>}
          <TrackList tracks={disc.tracks} queue={tracks} albumArtist={a.series_title} />
        </section>
      ))}
    </div>
  )
}
