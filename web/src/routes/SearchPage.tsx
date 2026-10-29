import { useNavigate, useSearch } from '@tanstack/react-router'
import { Search as SearchIcon, X } from 'lucide-react'
import { useEffect, useState } from 'react'
import { type ItemCard, useItems } from '@/api/queries/items'
import { PosterCard } from '@/components/media/PosterCard'
import { EmptyState } from '@/components/ui/EmptyState'

/** Search across movies, series and episodes, ranked by relevance (FTS5). */
export function SearchPage() {
  const { q = '' } = useSearch({ from: '/app/search' })
  const navigate = useNavigate({ from: '/search' })
  const [input, setInput] = useState(q)

  // Keep the URL in sync with the input, debounced so typing stays smooth.
  useEffect(() => {
    const term = input.trim()
    if (term === q) return
    const timer = setTimeout(() => void navigate({ search: term ? { q: term } : {}, replace: true }), 250)
    return () => clearTimeout(timer)
  }, [input, q, navigate])

  const enabled = q.length > 0
  const base = { search: q, sort: 'relevance' as const, limit: 40 }
  const movies = useItems({ ...base, kind: 'movie' }, enabled)
  const series = useItems({ ...base, kind: 'series' }, enabled)
  const episodes = useItems({ ...base, kind: 'episode', limit: 24 }, enabled)
  const loaded = movies.data && series.data && episodes.data
  const nothing = loaded && movies.data.total + series.data.total + episodes.data.total === 0

  return (
    <div className="px-6 pt-8 pb-16 md:px-10">
      <div className="relative mb-8 max-w-2xl">
        <SearchIcon className="pointer-events-none absolute top-1/2 left-4 size-5 -translate-y-1/2 text-subtle" aria-hidden="true" />
        <input
          type="search"
          autoFocus
          aria-label="Suche"
          placeholder="Filme, Serien, Folgen…"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          className="h-14 w-full rounded-full border border-line bg-surface-2 pr-12 pl-12 text-lg focus:border-accent focus:outline-none [&::-webkit-search-cancel-button]:hidden"
        />
        {input && (
          <button
            type="button"
            aria-label="Eingabe löschen"
            onClick={() => setInput('')}
            className="absolute top-1/2 right-4 -translate-y-1/2 text-subtle hover:text-fg"
          >
            <X className="size-5" />
          </button>
        )}
      </div>

      {!enabled && (
        <EmptyState icon={SearchIcon} title="Was möchtest du sehen?">
          Suche nach Titeln, Originaltiteln, Inhalten oder Folgennamen.
        </EmptyState>
      )}
      {nothing && (
        <EmptyState icon={SearchIcon} title="Keine Treffer">
          Für „{q}“ wurde nichts gefunden.
        </EmptyState>
      )}
      {enabled && (
        <div data-testid="search-results">
          <ResultGroup title="Filme" items={movies.data?.items} />
          <ResultGroup title="Serien" items={series.data?.items} />
          <ResultGroup title="Folgen" items={episodes.data?.items} wide />
        </div>
      )}
    </div>
  )
}

function ResultGroup({ title, items, wide = false }: { title: string; items?: ItemCard[]; wide?: boolean }) {
  if (!items?.length) return null
  return (
    <section className="mb-10" aria-label={title}>
      <h2 className="mb-4 text-lg font-bold">{title}</h2>
      <div
        className="grid gap-4"
        style={{ gridTemplateColumns: `repeat(auto-fill, minmax(${wide ? 240 : 150}px, 1fr))` }}
      >
        {items.map((item) => (
          <PosterCard key={item.id} item={item} wide={wide} />
        ))}
      </div>
    </section>
  )
}
