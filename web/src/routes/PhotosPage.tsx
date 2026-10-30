import { Link, useNavigate, useSearch } from '@tanstack/react-router'
import clsx from 'clsx'
import { ArrowLeft, Images, Play } from 'lucide-react'
import { useMemo, useState } from 'react'
import { type Photo, usePhotoAlbums, usePhotoTimeline } from '@/api/queries/photos'
import { BlurImage } from '@/components/media/BlurImage'
import { Lightbox } from '@/components/photos/Lightbox'
import { Button } from '@/components/ui/Button'
import { EmptyState } from '@/components/ui/EmptyState'
import { PageHeader } from '@/components/ui/PageHeader'
import { formatTimecode } from '@/lib/format'

const monthFormat = new Intl.DateTimeFormat('de', { month: 'long', year: 'numeric', timeZone: 'UTC' })

/** `YYYY-MM` of a capture time (EXIF local time, stored as UTC). */
function monthKey(ms: number): string {
  return new Date(ms).toISOString().slice(0, 7)
}

/** Pictures grouped by month, newest first (the list already is). */
function byMonth(photos: Photo[]): { key: string; label: string; start: number; photos: Photo[] }[] {
  const groups: { key: string; label: string; start: number; photos: Photo[] }[] = []
  photos.forEach((photo, index) => {
    const key = monthKey(photo.taken_at)
    const last = groups[groups.length - 1]
    if (last?.key === key) last.photos.push(photo)
    else groups.push({ key, label: monthFormat.format(photo.taken_at), start: index, photos: [photo] })
  })
  return groups
}

/** Photo libraries: the timeline by month, or the albums (folders). */
export function PhotosPage() {
  const { album } = useSearch({ from: '/app/photos' })
  const navigate = useNavigate({ from: '/photos' })
  const [view, setView] = useState<'timeline' | 'albums'>('timeline')
  const albums = usePhotoAlbums()
  const currentAlbum = album ? albums.data?.find((a) => a.id === album) : undefined

  return (
    <div className="pb-16">
      <PageHeader title={currentAlbum ? currentAlbum.title : 'Fotos'}>
        {album ? (
          <Button variant="ghost" className="h-9 px-4" onClick={() => void navigate({ search: {} })}>
            <ArrowLeft className="size-4" aria-hidden="true" /> Zurück
          </Button>
        ) : (
          <div className="flex gap-1 rounded-full bg-surface-2 p-1" role="tablist" aria-label="Ansicht">
            {(['timeline', 'albums'] as const).map((v) => (
              <button
                key={v}
                type="button"
                role="tab"
                aria-selected={view === v}
                onClick={() => setView(v)}
                className={clsx(
                  'rounded-full px-4 py-1.5 text-sm font-semibold',
                  view === v ? 'bg-fg text-canvas' : 'text-muted hover:text-fg',
                )}
              >
                {v === 'timeline' ? 'Zeitleiste' : 'Alben'}
              </button>
            ))}
          </div>
        )}
      </PageHeader>
      {view === 'albums' && !album ? <Albums /> : <Timeline albumId={album} />}
    </div>
  )
}

function Timeline({ albumId }: { albumId?: number }) {
  const timeline = usePhotoTimeline(albumId)
  const photos = useMemo(() => timeline.data?.pages.flatMap((p) => p.items) ?? [], [timeline.data])
  const groups = useMemo(() => byMonth(photos), [photos])
  const [open, setOpen] = useState<number | null>(null)

  if (timeline.isSuccess && photos.length === 0) {
    return (
      <EmptyState icon={Images} title="Noch keine Fotos">
        Lege in den Einstellungen eine Bibliothek vom Typ „Fotos“ an.
      </EmptyState>
    )
  }
  return (
    <div className="px-6 md:px-10" data-testid="photo-timeline">
      {groups.map((group) => (
        <section key={group.key} aria-label={group.label} className="mb-8">
          <h2 className="mb-3 text-lg font-bold" data-testid="photo-month">
            {group.label}
          </h2>
          <div className="grid grid-cols-[repeat(auto-fill,minmax(9rem,1fr))] gap-1.5">
            {group.photos.map((photo, i) => (
              <button
                key={photo.id}
                type="button"
                onClick={() => setOpen(group.start + i)}
                aria-label={photo.title}
                className="group relative aspect-square overflow-hidden rounded-md focus-visible:outline-2"
                data-testid="photo-thumb"
              >
                <BlurImage
                  src={photo.thumb_url}
                  blurhash={photo.blurhash}
                  alt=""
                  className="absolute inset-0 transition-transform duration-300 group-hover:scale-105"
                />
                {photo.kind === 'video' && (
                  <span className="absolute right-1.5 bottom-1.5 flex items-center gap-1 rounded bg-black/70 px-1.5 py-0.5 text-xs font-semibold text-white">
                    <Play className="size-3 fill-current" aria-hidden="true" />
                    {photo.duration_ms ? formatTimecode(photo.duration_ms) : 'Video'}
                  </span>
                )}
              </button>
            ))}
          </div>
        </section>
      ))}
      {timeline.hasNextPage && (
        <div className="flex justify-center">
          <Button variant="ghost" loading={timeline.isFetchingNextPage} onClick={() => void timeline.fetchNextPage()}>
            Mehr laden
          </Button>
        </div>
      )}
      {open !== null && photos[open] && (
        <Lightbox photos={photos} index={open} onIndex={setOpen} onClose={() => setOpen(null)} />
      )}
    </div>
  )
}

function Albums() {
  const albums = usePhotoAlbums()
  if (albums.isSuccess && albums.data.length === 0) {
    return <EmptyState icon={Images} title="Keine Alben">Alben entstehen aus den Ordnern der Fotobibliothek.</EmptyState>
  }
  return (
    <ul className="grid grid-cols-[repeat(auto-fill,minmax(12rem,1fr))] gap-5 px-6 md:px-10" data-testid="photo-albums">
      {albums.data?.map((album) => (
        <li key={album.id}>
          <Link to="/photos" search={{ album: album.id }} className="group block">
            <BlurImage
              src={album.cover_url}
              blurhash={album.cover_blurhash}
              alt=""
              className="aspect-square rounded-[var(--radius-card)] transition-opacity group-hover:opacity-90"
            />
            <p className="mt-2 truncate text-sm font-bold">{album.title}</p>
            <p className="text-xs text-subtle">
              {album.photo_count} {album.photo_count === 1 ? 'Foto' : 'Fotos'}
              {album.last_taken_at ? ` · ${monthFormat.format(album.last_taken_at)}` : ''}
            </p>
          </Link>
        </li>
      ))}
    </ul>
  )
}
