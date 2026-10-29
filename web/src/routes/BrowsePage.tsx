import { useNavigate, useSearch } from '@tanstack/react-router'
import clsx from 'clsx'
import { ArrowDownAZ, ArrowUpZA, Clapperboard, Film } from 'lucide-react'
import { useRef } from 'react'
import { type ItemsParams, useGenres, useItems, useLetters } from '@/api/queries/items'
import { EmptyState } from '@/components/ui/EmptyState'
import { PageHeader } from '@/components/ui/PageHeader'
import { VirtualGrid, type VirtualGridHandle } from '@/components/media/VirtualGrid'

export interface BrowseSearch {
  sort?: 'title' | 'date_added' | 'year' | 'rating'
  order?: 'asc' | 'desc'
  genre?: string
  played?: 'yes' | 'no'
}

const SORTS: { value: NonNullable<BrowseSearch['sort']>; label: string }[] = [
  { value: 'title', label: 'Titel' },
  { value: 'date_added', label: 'Hinzugefügt' },
  { value: 'year', label: 'Erscheinungsjahr' },
  { value: 'rating', label: 'Bewertung' },
]

const selectClass =
  'h-9 rounded-full border border-line bg-surface-2 px-3 text-sm text-fg focus:border-accent focus:outline-none'

/** Grid of all movies (or all series) the user may see, with sort, filters and A–Z. */
export function BrowsePage({ kind, from }: { kind: 'movie' | 'series'; from: '/movies' | '/shows' }) {
  const search = useSearch({ from: from === '/movies' ? '/app/movies' : '/app/shows' }) as BrowseSearch
  const navigate = useNavigate({ from })
  const grid = useRef<VirtualGridHandle>(null)
  const genres = useGenres()

  const sort = search.sort ?? 'title'
  const order = search.order ?? (sort === 'title' ? 'asc' : 'desc')
  const params: ItemsParams = {
    kind,
    sort,
    order,
    genre: search.genre,
    played: search.played === 'yes' ? true : search.played === 'no' ? false : undefined,
  }
  const first = useItems({ ...params, limit: 60 })
  const letters = useLetters(params)
  const total = first.data?.total ?? 0
  const title = kind === 'movie' ? 'Filme' : 'Serien'

  const set = (patch: Partial<BrowseSearch>) =>
    void navigate({ search: (prev: BrowseSearch) => ({ ...prev, ...patch }), replace: true })

  return (
    <div className="pb-16">
      <PageHeader title={title}>
        <span className="text-sm text-muted" data-testid="browse-total">
          {first.data ? `${total} ${kind === 'movie' ? (total === 1 ? 'Film' : 'Filme') : total === 1 ? 'Serie' : 'Serien'}` : ''}
        </span>
      </PageHeader>

      <div className="mb-6 flex flex-wrap items-center gap-2 px-6 md:px-10">
        <select aria-label="Sortierung" className={selectClass} value={sort} onChange={(e) => set({ sort: e.target.value as BrowseSearch['sort'], order: undefined })}>
          {SORTS.map((s) => (
            <option key={s.value} value={s.value}>
              {s.label}
            </option>
          ))}
        </select>
        <button
          type="button"
          aria-label={order === 'asc' ? 'Aufsteigend' : 'Absteigend'}
          title={order === 'asc' ? 'Aufsteigend' : 'Absteigend'}
          onClick={() => set({ order: order === 'asc' ? 'desc' : 'asc' })}
          className="flex size-9 items-center justify-center rounded-full border border-line bg-surface-2 text-muted hover:text-fg"
        >
          {order === 'asc' ? <ArrowDownAZ className="size-4" /> : <ArrowUpZA className="size-4" />}
        </button>
        <select aria-label="Genre" className={selectClass} value={search.genre ?? ''} onChange={(e) => set({ genre: e.target.value || undefined })}>
          <option value="">Alle Genres</option>
          {genres.data?.map((g) => (
            <option key={g.name} value={g.name}>
              {g.name}
            </option>
          ))}
        </select>
        <select aria-label="Gesehen" className={selectClass} value={search.played ?? ''} onChange={(e) => set({ played: (e.target.value || undefined) as BrowseSearch['played'] })}>
          <option value="">Alle</option>
          <option value="no">Ungesehen</option>
          <option value="yes">Gesehen</option>
        </select>
      </div>

      {first.data && total === 0 ? (
        <EmptyState icon={kind === 'movie' ? Film : Clapperboard} title={`Keine ${title} gefunden`}>
          {search.genre || search.played ? 'Passe die Filter an.' : 'Füge eine Bibliothek hinzu oder warte, bis der Scan fertig ist.'}
        </EmptyState>
      ) : (
        <div className="flex gap-2 pl-6 md:pl-10">
          <div className="min-w-0 flex-1">
            {first.data && <VirtualGrid ref={grid} params={params} total={total} />}
          </div>
          {sort === 'title' && order === 'asc' && (letters.data?.length ?? 0) > 1 && (
            <nav aria-label="Springe zu Buchstabe" className="sticky top-4 flex h-fit w-8 shrink-0 flex-col items-center pr-1 md:mr-4">
              {letters.data!.map((l) => (
                <button
                  key={l.letter}
                  type="button"
                  onClick={() => grid.current?.scrollToOffset(l.offset)}
                  className={clsx('w-full rounded py-0.5 text-[11px] font-bold text-subtle hover:bg-surface-2 hover:text-accent')}
                >
                  {l.letter}
                </button>
              ))}
            </nav>
          )}
        </div>
      )}
    </div>
  )
}
