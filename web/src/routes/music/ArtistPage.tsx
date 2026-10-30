import { useParams } from '@tanstack/react-router'
import { MicVocal, Play, Shuffle } from 'lucide-react'
import { fetchArtistTracks, useArtist } from '@/api/queries/music'
import { BlurImage } from '@/components/media/BlurImage'
import { AlbumCard, CardGrid } from '@/components/music/MusicCards'
import { TrackList } from '@/components/music/TrackList'
import { Button } from '@/components/ui/Button'
import { EmptyState } from '@/components/ui/EmptyState'
import { musicPlayer } from '@/lib/music/player'

/** An artist: popular tracks, albums, appearances on compilations. */
export function ArtistPage() {
  const { id } = useParams({ from: '/app/music/artist/$id' })
  const artist = useArtist(Number(id))

  if (artist.isError) {
    return (
      <div className="pt-10">
        <EmptyState icon={MicVocal} title="Künstler nicht gefunden" />
      </div>
    )
  }
  if (!artist.data) return null
  const a = artist.data

  async function playAll(shuffle: boolean) {
    const tracks = await fetchArtistTracks(a.id)
    musicPlayer.playTracks(tracks, 0, { shuffle })
  }

  return (
    <div className="pb-16">
      <header className="relative overflow-hidden px-6 pt-16 pb-8 md:px-10">
        {a.backdrop_url && (
          <>
            <BlurImage src={a.backdrop_url} alt="" className="absolute inset-0 opacity-40" eager />
            <div className="absolute inset-0 bg-gradient-to-t from-canvas via-canvas/60 to-transparent" />
          </>
        )}
        <div className="relative flex flex-col gap-6 md:flex-row md:items-end">
          <div className="relative aspect-square w-40 shrink-0 overflow-hidden rounded-full shadow-[var(--shadow-card)] md:w-52">
            <BlurImage src={a.poster_url} blurhash={a.poster_blurhash} alt={a.title} className="absolute inset-0" eager />
            {!a.poster_url && <MicVocal className="absolute inset-0 m-auto size-12 text-subtle" aria-hidden="true" />}
          </div>
          <div className="min-w-0">
            <p className="text-xs font-bold tracking-widest text-subtle uppercase">Künstler</p>
            {a.logo_url ? (
              <h1 className="mt-2">
                <img src={a.logo_url} alt={a.title} className="max-h-24 max-w-sm object-contain" />
              </h1>
            ) : (
              <h1 className="mt-1 text-3xl font-extrabold tracking-tight md:text-5xl">{a.title}</h1>
            )}
            <p className="mt-2 text-sm text-subtle">
              {a.albums.length} {a.albums.length === 1 ? 'Album' : 'Alben'}
            </p>
            <div className="mt-5 flex flex-wrap gap-3">
              <Button variant="accent" onClick={() => void playAll(false)}>
                <Play className="size-4" fill="currentColor" aria-hidden="true" />
                Abspielen
              </Button>
              <Button variant="ghost" onClick={() => void playAll(true)}>
                <Shuffle className="size-4" aria-hidden="true" />
                Zufällig
              </Button>
            </div>
          </div>
        </div>
      </header>
      <div className="flex flex-col gap-10 px-6 md:px-10">
        {a.popular.length > 0 && (
          <section aria-label="Beliebte Titel">
            <h2 className="mb-3 text-lg font-bold">Beliebte Titel</h2>
            <TrackList tracks={a.popular} showAlbum />
          </section>
        )}
        {a.albums.length > 0 && (
          <section aria-label="Alben">
            <h2 className="mb-4 text-lg font-bold">Alben</h2>
            <CardGrid>
              {a.albums.map((album) => (
                <AlbumCard key={album.id} album={album} showArtist={false} />
              ))}
            </CardGrid>
          </section>
        )}
        {a.appears_on.length > 0 && (
          <section aria-label="Auch enthalten auf">
            <h2 className="mb-4 text-lg font-bold">Auch enthalten auf</h2>
            <CardGrid>
              {a.appears_on.map((album) => (
                <AlbumCard key={album.id} album={album} />
              ))}
            </CardGrid>
          </section>
        )}
      </div>
    </div>
  )
}
