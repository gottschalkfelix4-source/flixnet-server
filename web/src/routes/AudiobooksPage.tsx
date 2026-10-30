import { Link } from '@tanstack/react-router'
import { BookAudio } from 'lucide-react'
import { useState } from 'react'
import { useAudiobooks } from '@/api/queries/audiobooks'
import { BlurImage } from '@/components/media/BlurImage'
import { EmptyState } from '@/components/ui/EmptyState'
import { PageHeader } from '@/components/ui/PageHeader'

/** All audiobooks by author and series, with listening progress. */
export function AudiobooksPage() {
  const [search, setSearch] = useState('')
  const books = useAudiobooks(search.trim())

  return (
    <div className="pb-16">
      <PageHeader title="Hörbücher">
        <input
          type="search"
          aria-label="Hörbücher durchsuchen"
          placeholder="Titel, Autor, Sprecher …"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          className="h-9 w-56 rounded-full border border-line bg-surface-2 px-4 text-sm focus:border-accent focus:outline-none"
        />
      </PageHeader>
      {books.isSuccess && books.data.items.length === 0 ? (
        <EmptyState icon={BookAudio} title={search ? 'Nichts gefunden' : 'Noch keine Hörbücher'}>
          {search ? 'Kein Hörbuch passt zur Suche.' : 'Lege in den Einstellungen eine Bibliothek vom Typ „Hörbücher“ an.'}
        </EmptyState>
      ) : (
        <ul className="grid grid-cols-[repeat(auto-fill,minmax(10rem,1fr))] gap-6 px-6 md:px-10" data-testid="audiobook-grid">
          {books.data?.items.map((book) => {
            const progress =
              book.duration_ms && book.position_ms > 0 ? Math.min(100, (book.position_ms / book.duration_ms) * 100) : 0
            return (
              <li key={book.id}>
                <Link to="/audiobook/$id" params={{ id: String(book.id) }} className="group block" data-testid="audiobook-card">
                  <div className="relative">
                    <BlurImage
                      src={book.cover_url}
                      blurhash={book.cover_blurhash}
                      alt=""
                      className="aspect-square rounded-[var(--radius-card)] transition-opacity group-hover:opacity-90"
                    />
                    {!book.cover_url && (
                      <BookAudio className="absolute inset-0 m-auto size-12 text-subtle" aria-hidden="true" />
                    )}
                    {progress > 0 && (
                      <div className="absolute inset-x-2 bottom-2 h-1 overflow-hidden rounded-full bg-black/60">
                        <div className="h-full bg-accent" style={{ width: `${progress}%` }} />
                      </div>
                    )}
                  </div>
                  <p className="mt-2 line-clamp-2 text-sm font-bold">{book.title}</p>
                  <p className="truncate text-xs text-subtle">
                    {[book.author, book.series_name && `${book.series_name}${book.series_index ? ` ${book.series_index}` : ''}`]
                      .filter(Boolean)
                      .join(' · ')}
                  </p>
                </Link>
              </li>
            )
          })}
        </ul>
      )}
    </div>
  )
}
