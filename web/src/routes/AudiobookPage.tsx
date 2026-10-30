import { Link, useParams } from '@tanstack/react-router'
import { ArrowLeft, BookAudio } from 'lucide-react'
import { useAudiobook } from '@/api/queries/audiobooks'
import { AudiobookPlayer } from '@/components/audiobooks/AudiobookPlayer'
import { BlurImage } from '@/components/media/BlurImage'
import { formatTimecode } from '@/lib/format'

/** A book: cover, details, chapters and its own player. */
export function AudiobookPage() {
  const { id } = useParams({ from: '/app/audiobook/$id' })
  const book = useAudiobook(Number(id))

  if (book.isError) {
    return <p className="p-10 text-muted">Dieses Hörbuch gibt es nicht (mehr).</p>
  }
  if (!book.data) return null
  const b = book.data
  return (
    <div className="mx-auto max-w-5xl px-6 pt-6 pb-16 md:px-10">
      <Link to="/audiobooks" className="mb-6 inline-flex items-center gap-2 text-sm text-muted hover:text-fg">
        <ArrowLeft className="size-4" aria-hidden="true" /> Alle Hörbücher
      </Link>
      <div className="grid gap-8 md:grid-cols-[16rem_1fr]">
        <div className="relative">
          <BlurImage
            src={b.large_cover_url}
            blurhash={b.cover_blurhash}
            alt=""
            eager
            className="aspect-square w-full rounded-[var(--radius-card)] shadow-[var(--shadow-card)]"
          />
          {!b.large_cover_url && <BookAudio className="absolute inset-0 m-auto size-16 text-subtle" aria-hidden="true" />}
        </div>
        <div className="min-w-0">
          <h1 className="text-3xl font-extrabold" data-testid="audiobook-title">
            {b.title}
          </h1>
          <p className="mt-2 text-muted">
            {[b.author && `von ${b.author}`, b.narrator && `gelesen von ${b.narrator}`].filter(Boolean).join(' · ')}
          </p>
          <p className="mt-1 text-sm text-subtle">
            {[
              b.series_name && `${b.series_name}${b.series_index ? `, Band ${b.series_index}` : ''}`,
              b.year,
              b.duration_ms ? formatTimecode(b.duration_ms) : null,
              `${b.parts.length} ${b.parts.length === 1 ? 'Datei' : 'Dateien'}`,
            ]
              .filter(Boolean)
              .join(' · ')}
          </p>
          {b.overview && <p className="mt-4 max-w-2xl text-sm leading-relaxed text-muted">{b.overview}</p>}
          <div className="mt-6">
            <AudiobookPlayer key={b.id} book={b} />
          </div>
        </div>
      </div>
    </div>
  )
}
