import { useNavigate, useSearch } from '@tanstack/react-router'
import clsx from 'clsx'
import { ArrowDown, ArrowUp, Clapperboard, Film } from 'lucide-react'
import { useRef } from 'react'
import { type ItemsParams, useGenres, useItems, useLetters } from '@/api/queries/items'
import { EmptyState } from '@/components/ui/EmptyState'
import { FilterMenu } from '@/components/ui/FilterMenu'
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

const PLAYED: { value: 'all' | 'no' | 'yes'; label: string }[] = [
  { value: 'all', label: 'Alle' },
  { value: 'no', label: 'Ungesehen' },
  { value: 'yes', label: 'Gesehen' },
]

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
      <PageHeader title={title} />

      {/* Filter bar like a Plex library: "Alle ▾  Alle Genres ▾  Nach Titel ▾  [123]". */}
      <div className="mb-5 flex flex-wrap items-center gap-1 px-4 md:px-8">
        <FilterMenu
          name="Gesehen"
          label={PLAYED.find((p) => p.value === (search.played ?? 'all'))!.label}
          value={search.played ?? 'all'}
          options={PLAYED}
          onSelect={(v) => set({ played: v === 'all' ? undefined : v })}
        />
        <FilterMenu
          name="Genre"
          label={search.genre ?? 'Alle Genres'}
          value={search.genre ?? ''}
          options={[{ value: '', label: 'Alle Genres' }, ...(genres.data?.map((g) => ({ value: g.name, label: g.name })) ?? [])]}
          onSelect={(v) => set({ genre: v || undefined })}
        />
        <FilterMenu
          name="Sortierung"
          label={`Nach ${SORTS.find((s) => s.value === sort)!.label}`}
          value={sort}
          options={SORTS.map((s) => ({
            ...s,
            hint:
              s.value === sort ? (
                order === 'asc' ? (
                  <ArrowUp className="size-4" aria-label="aufsteigend" />
                ) : (
                  <ArrowDown className="size-4" aria-label="absteigend" />
                )
              ) : undefined,
          }))}
          // Choosing the current sort again flips its direction (as in Plex).
          onSelect={(v) => set(v === sort ? { order: order === 'asc' ? 'desc' : 'asc' } : { sort: v, order: undefined })}
        />
        {first.data && (
          <span className="ml-1 rounded-md bg-surface-2 px-2 py-0.5 text-sm font-semibold text-muted tabular-nums" data-testid="browse-total" title={`${total} ${kind === 'movie' ? (total === 1 ? 'Film' : 'Filme') : total === 1 ? 'Serie' : 'Serien'}`}>
            {total}
          </span>
        )}
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
            <nav aria-label="Springe zu Buchstabe" className="sticky top-18 flex h-fit w-8 shrink-0 flex-col items-center pr-1 md:mr-4">
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
