import { Link, useNavigate, useSearch } from '@tanstack/react-router'
import clsx from 'clsx'
import { Clapperboard, MessageSquareQuote, Play, Search as SearchIcon, Sparkles, X } from 'lucide-react'
import { useEffect, useState } from 'react'
import { type NaturalSearch, type SceneHit, useNaturalSearch, useSceneSearch } from '@/api/queries/assistant'
import { type ItemCard, useItems } from '@/api/queries/items'
import { useTracks } from '@/api/queries/music'
import { BlurImage } from '@/components/media/BlurImage'
import { PosterCard } from '@/components/media/PosterCard'
import { AlbumCard, ArtistCard, CardGrid } from '@/components/music/MusicCards'
import { TrackList } from '@/components/music/TrackList'
import { EmptyState } from '@/components/ui/EmptyState'
import { formatTimecode } from '@/lib/format'
import { episodeLabel } from '@/lib/media'

export type SearchMode = 'titles' | 'scenes' | 'natural'

interface ModeInfo {
  id: SearchMode
  label: string
  placeholder: string
  icon: typeof SearchIcon
}

const TITLES: ModeInfo = { id: 'titles', label: 'Titel', placeholder: 'Filme, Serien, Folgen, Musik…', icon: SearchIcon }
const MODES: ModeInfo[] = [
  TITLES,
  { id: 'scenes', label: 'Szenen', placeholder: 'z. B. die Szene, wo sie am Strand streiten', icon: MessageSquareQuote },
  { id: 'natural', label: 'Frei formuliert', placeholder: 'z. B. lustige Filme aus den 90ern unter 2 Stunden', icon: Sparkles },
]

/** Search across movies, series and episodes: by title (FTS5), by dialogue (scene
 *  search) or in natural language (LLM filter, keyword fallback). */
export function SearchPage() {
  const { q = '', mode = 'titles' } = useSearch({ from: '/app/search' })
  const navigate = useNavigate({ from: '/search' })
  const [input, setInput] = useState(q)

  // Keep the URL in sync with the input, debounced so typing stays smooth (free text
  // waits longer: every query costs an LLM call).
  useEffect(() => {
    const term = input.trim()
    if (term === q) return
    const timer = setTimeout(
      () => void navigate({ search: { ...(term ? { q: term } : {}), ...(mode !== 'titles' ? { mode } : {}) }, replace: true }),
      mode === 'natural' ? 700 : 250,
    )
    return () => clearTimeout(timer)
  }, [input, q, mode, navigate])

  const current = MODES.find((m) => m.id === mode) ?? TITLES
  const enabled = q.length > 0

  return (
    <div className="px-6 pt-8 pb-16 md:px-10">
      <div className="mb-4 flex flex-wrap gap-2" role="tablist" aria-label="Suchart">
        {MODES.map((m) => (
          <button
            key={m.id}
            type="button"
            role="tab"
            aria-selected={m.id === mode}
            onClick={() =>
              void navigate({ search: { ...(q ? { q } : {}), ...(m.id !== 'titles' ? { mode: m.id } : {}) }, replace: true })
            }
            className={clsx(
              'inline-flex items-center gap-1.5 rounded-full px-4 py-2 text-sm font-semibold transition-colors',
              m.id === mode ? 'bg-fg text-canvas' : 'text-muted hover:bg-surface-2 hover:text-fg',
            )}
          >
            <m.icon className="size-4" aria-hidden="true" />
            {m.label}
          </button>
        ))}
      </div>
      <div className="relative mb-8 max-w-2xl">
        <current.icon className="pointer-events-none absolute top-1/2 left-4 size-5 -translate-y-1/2 text-subtle" aria-hidden="true" />
        <input
          type="search"
          autoFocus
          aria-label="Suche"
          placeholder={current.placeholder}
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

      {!enabled && <Intro mode={mode} />}
      {enabled && mode === 'titles' && <TitleResults q={q} />}
      {enabled && mode === 'scenes' && <SceneResults q={q} />}
      {enabled && mode === 'natural' && <NaturalResults q={q} />}
    </div>
  )
}

function Intro({ mode }: { mode: SearchMode }) {
  if (mode === 'scenes')
    return (
      <EmptyState icon={MessageSquareQuote} title="Welche Szene suchst du?">
        Beschreibe, was gesagt wird oder passiert – gesucht wird in den Untertiteln aller Filme und Folgen.
      </EmptyState>
    )
  if (mode === 'natural')
    return (
      <EmptyState icon={Sparkles} title="Was möchtest du sehen?">
        Beschreibe in eigenen Worten, worauf du Lust hast. Mit eingerichtetem Sprachmodell wird daraus ein Filter, sonst eine Stichwortsuche.
      </EmptyState>
    )
  return (
    <EmptyState icon={SearchIcon} title="Was möchtest du sehen?">
      Suche nach Titeln, Originaltiteln, Inhalten oder Folgennamen.
    </EmptyState>
  )
}

function TitleResults({ q }: { q: string }) {
  const base = { search: q, sort: 'relevance' as const, limit: 40 }
  const movies = useItems({ ...base, kind: 'movie' })
  const series = useItems({ ...base, kind: 'series' })
  const episodes = useItems({ ...base, kind: 'episode', limit: 24 })
  const artists = useItems({ ...base, kind: 'artist', limit: 12 })
  const albums = useItems({ ...base, kind: 'album', limit: 24 })
  const tracks = useTracks({ search: q, limit: 20 }, true)
  const loaded = movies.data && series.data && episodes.data && artists.data && albums.data && tracks.data
  const nothing =
    loaded &&
    movies.data.total + series.data.total + episodes.data.total + artists.data.total + albums.data.total + tracks.data.total === 0
  if (nothing)
    return (
      <EmptyState icon={SearchIcon} title="Keine Treffer">
        Für „{q}“ wurde nichts gefunden.
      </EmptyState>
    )
  return (
    <div data-testid="search-results">
      <ResultGroup title="Filme" items={movies.data?.items} />
      <ResultGroup title="Serien" items={series.data?.items} />
      <ResultGroup title="Folgen" items={episodes.data?.items} wide />
      {!!artists.data?.items.length && (
        <section className="mb-10" aria-label="Künstler">
          <h2 className="mb-4 text-lg font-bold">Künstler</h2>
          <CardGrid round>
            {artists.data.items.map((a) => (
              <ArtistCard key={a.id} artist={a} />
            ))}
          </CardGrid>
        </section>
      )}
      {!!albums.data?.items.length && (
        <section className="mb-10" aria-label="Alben">
          <h2 className="mb-4 text-lg font-bold">Alben</h2>
          <CardGrid>
            {albums.data.items.map((a) => (
              <AlbumCard key={a.id} album={a} />
            ))}
          </CardGrid>
        </section>
      )}
      {!!tracks.data?.items.length && (
        <section className="mb-10" aria-label="Titel">
          <h2 className="mb-4 text-lg font-bold">Titel</h2>
          <TrackList tracks={tracks.data.items} showAlbum />
        </section>
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

function SceneResults({ q }: { q: string }) {
  const scenes = useSceneSearch(q)
  if (!scenes.data) return <div className="h-40 animate-pulse rounded-2xl bg-surface/40" />
  if (scenes.data.hits.length === 0)
    return (
      <EmptyState icon={MessageSquareQuote} title="Keine Szene gefunden">
        In den Dialogen kommt „{q}“ nicht vor. Filme ohne Untertitel lassen sich in den Einstellungen per Whisper transkribieren.
      </EmptyState>
    )
  return (
    <ol className="flex max-w-4xl flex-col gap-3" data-testid="scene-results" aria-label="Szenen">
      {scenes.data.hits.map((hit) => (
        <SceneRow key={`${hit.item.id}-${hit.start_ms}`} hit={hit} terms={scenes.data.terms} />
      ))}
    </ol>
  )
}

/** The snippet with the search terms highlighted. */
function Highlighted({ text, terms }: { text: string; terms: string[] }) {
  if (terms.length === 0) return <>{text}</>
  const escaped = terms.map((t) => t.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'))
  const parts = text.split(new RegExp(`(\\p{L}*(?:${escaped.join('|')})\\p{L}*)`, 'giu'))
  return (
    <>
      {parts.map((part, i) =>
        i % 2 === 1 ? (
          <mark key={i} className="rounded bg-accent/25 px-0.5 text-fg">
            {part}
          </mark>
        ) : (
          part
        ),
      )}
    </>
  )
}

function SceneRow({ hit, terms }: { hit: SceneHit; terms: string[] }) {
  const item = hit.item
  const label = episodeLabel(item)
  const heading = item.kind === 'episode' && item.series_title ? item.series_title : item.title
  const sub = item.kind === 'episode' ? [label, item.title].filter(Boolean).join(' · ') : item.year
  return (
    <li className="flex gap-4 rounded-2xl border border-line/70 bg-surface/70 p-3" data-testid="scene-hit">
      <Link to="/item/$id" params={{ id: String(item.id) }} className="w-28 shrink-0 overflow-hidden rounded-lg">
        <BlurImage src={item.backdrop_url ?? item.poster_url} blurhash={item.poster_blurhash} alt="" className="aspect-video" />
      </Link>
      <div className="flex min-w-0 flex-1 flex-col gap-1">
        <p className="truncate font-bold">
          {heading}
          {sub && <span className="ml-2 text-sm font-normal text-muted">{sub}</span>}
        </p>
        <p className="text-sm leading-relaxed text-fg/85">
          <span className="mr-2 font-mono text-xs text-accent">{formatTimecode(hit.start_ms)}</span>
          „<Highlighted text={hit.snippet} terms={terms} />“
        </p>
      </div>
      <Link
        to="/play/$id"
        params={{ id: String(item.id) }}
        search={{ start: hit.start_ms }}
        className="inline-flex shrink-0 items-center gap-1.5 self-center rounded-full bg-accent px-4 py-2 text-sm font-bold text-accent-fg hover:bg-accent-hover"
      >
        <Play className="size-4 fill-current" aria-hidden="true" />
        An dieser Stelle abspielen
      </Link>
    </li>
  )
}

function chips(n: NaturalSearch): string[] {
  const i = n.interpreted
  if (!i) return []
  const kinds: Record<string, string> = { movie: 'Filme', series: 'Serien', episode: 'Folgen' }
  const out: string[] = []
  out.push(...i.kinds.map((k) => kinds[k] ?? k))
  out.push(...i.genres)
  if (i.year_from != null || i.year_to != null) {
    if (i.year_from != null && i.year_to != null) out.push(i.year_from === i.year_to ? `${i.year_from}` : `${i.year_from}–${i.year_to}`)
    else if (i.year_from != null) out.push(`ab ${i.year_from}`)
    else out.push(`bis ${i.year_to}`)
  }
  if (i.max_runtime_minutes != null) out.push(`bis ${i.max_runtime_minutes} Min.`)
  if (i.min_runtime_minutes != null) out.push(`ab ${i.min_runtime_minutes} Min.`)
  if (i.min_rating != null) out.push(`ab ${i.min_rating} ★`)
  if (i.played === false) out.push('noch nicht gesehen')
  if (i.played === true) out.push('schon gesehen')
  if (i.favorite) out.push('Favoriten')
  out.push(...i.people)
  if (i.search_text) out.push(`„${i.search_text}“`)
  return out
}

function NaturalResults({ q }: { q: string }) {
  const search = useNaturalSearch(q)
  if (!search.data)
    return (
      <p className="flex items-center gap-2 text-sm text-muted" role="status">
        <Sparkles className="size-4 animate-pulse" aria-hidden="true" />
        Suche wird verstanden…
      </p>
    )
  const n = search.data
  return (
    <div data-testid="natural-results">
      <div className="mb-6 flex flex-wrap items-center gap-2" data-testid="natural-filter">
        <span className="text-sm text-muted">
          {n.mode === 'llm' ? 'Verstanden als:' : 'Stichwortsuche'}
        </span>
        {chips(n).map((chip) => (
          <span key={chip} className="rounded-full border border-accent/40 bg-accent/10 px-3 py-1 text-xs font-semibold">
            {chip}
          </span>
        ))}
      </div>
      {n.notice && <p className="mb-4 text-sm text-amber-300">{n.notice}</p>}
      {n.items.length === 0 ? (
        <EmptyState icon={Clapperboard} title="Nichts Passendes">
          Versuche es mit weniger Bedingungen.
        </EmptyState>
      ) : (
        <ResultGroup title={`${n.total} Treffer`} items={n.items} />
      )}
    </div>
  )
}
