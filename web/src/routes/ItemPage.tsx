import { Link, Navigate, useParams, useSearch } from '@tanstack/react-router'
import clsx from 'clsx'
import { Captions, Check, ChevronLeft, Heart, Layers, RefreshCw, Search, Star } from 'lucide-react'
import { useState } from 'react'
import { type ItemCard, type ItemDetail, useItem, useItems, useRefreshItem, useSetFavorite, useSetPlayed } from '@/api/queries/items'
import { AssistantInsights } from '@/components/assistant/AssistantInsights'
import { AddToCollectionDialog } from '@/components/media/AddToCollectionDialog'
import { AddToPlaylistButton } from '@/components/media/AddToPlaylistDialog'
import { BlurImage } from '@/components/media/BlurImage'
import { CastRow } from '@/components/media/CastRow'
import { CollectionRows } from '@/components/media/CollectionRows'
import { IdentifyDialog } from '@/components/media/IdentifyDialog'
import { PlayActions } from '@/components/media/PlayActions'
import { Row } from '@/components/media/Row'
import { SegmentsPanel } from '@/components/media/SegmentsPanel'
import { SubtitleSearchDialog } from '@/components/media/SubtitleSearchDialog'
import { episodeLabel, formatBadges, formatRuntime, languageName } from '@/lib/media'
import { useSession } from '@/lib/session'
import { NotFoundPage } from './NotFoundPage'

export function ItemPage() {
  const { id } = useParams({ from: '/app/item/$id' })
  const { season } = useSearch({ from: '/app/item/$id' })
  const item = useItem(Number(id))

  if (item.isError) return <NotFoundPage />
  if (!item.data) return <div className="h-[60vh] animate-pulse bg-surface/40" />
  const d = item.data

  // Seasons open inside their series page.
  if (d.kind === 'season' && d.series_id) {
    return (
      <Navigate to="/item/$id" params={{ id: String(d.series_id) }} search={{ season: d.index_number ?? undefined }} replace />
    )
  }

  return (
    // Keyed so local state (season tab, dialogs) resets when navigating between items.
    <div key={d.id} className="pb-16">
      <Header item={d} />
      <AssistantInsights item={d} />
      {d.kind === 'series' && <Seasons series={d} initial={season} />}
      <CastRow people={d.people} />
      {(d.kind === 'movie' || d.kind === 'series') && <CollectionRows itemId={d.id} />}
      {d.kind === 'episode' && <SegmentsPanel itemId={d.id} />}
      <TechnicalInfo item={d} />
      {d.kind !== 'episode' && <MoreLikeThis item={d} />}
    </div>
  )
}

function Header({ item }: { item: ItemDetail }) {
  const session = useSession()
  const setPlayed = useSetPlayed()
  const setFavorite = useSetFavorite()
  const refresh = useRefreshItem()
  const [identifying, setIdentifying] = useState(false)
  const [collecting, setCollecting] = useState(false)
  const [searchingSubtitles, setSearchingSubtitles] = useState(false)
  const badges = formatBadges(item)
  const runtime = formatRuntime(item.runtime_ms)
  const label = episodeLabel(item)
  const backdrop = item.artwork.backdrop_url ?? item.artwork.thumb_url
  const meta = [
    label,
    item.year,
    item.official_rating,
    item.kind === 'series' && item.child_count > 0 ? `${item.child_count} ${item.child_count === 1 ? 'Staffel' : 'Staffeln'}` : runtime,
  ].filter(Boolean)
  const directors = item.people.filter((p) => p.role === 'director' || p.role === 'creator').map((p) => p.name)

  return (
    <section className="relative isolate min-h-[70vh]" data-testid="item-header">
      <div aria-hidden="true" className="absolute inset-0 -z-10 overflow-hidden">
        <BlurImage src={backdrop} blurhash={item.artwork.backdrop_blurhash} alt="" eager className="size-full brightness-75" />
        <div className="absolute inset-0 bg-gradient-to-t from-canvas via-canvas/70 to-canvas/10" />
        <div className="absolute inset-0 bg-gradient-to-r from-canvas/95 via-canvas/50 to-transparent" />
      </div>

      <div className="flex min-h-[70vh] flex-col justify-end gap-8 px-6 pt-20 pb-10 md:flex-row md:items-end md:justify-start md:px-10">
        {item.kind !== 'episode' && (
          <div className="hidden w-56 shrink-0 overflow-hidden rounded-[var(--radius-card)] shadow-2xl md:block lg:w-64">
            <BlurImage src={item.artwork.poster_url} blurhash={item.artwork.poster_blurhash} alt="" eager className="aspect-[2/3]" />
          </div>
        )}
        <div className="flex max-w-3xl flex-col gap-4">
          {item.kind === 'episode' && item.series_id && (
            <Link
              to="/item/$id"
              params={{ id: String(item.series_id) }}
              search={{ season: item.parent_index_number ?? undefined }}
              className="inline-flex items-center gap-1 text-sm font-semibold text-muted hover:text-fg"
            >
              <ChevronLeft className="size-4" aria-hidden="true" />
              {item.series_title}
            </Link>
          )}
          {item.artwork.logo_url && item.kind !== 'episode' ? (
            <img src={item.artwork.logo_url} alt={item.title} className="max-h-32 max-w-[min(28rem,90%)] object-contain object-left" />
          ) : (
            <h1 className="text-4xl font-extrabold tracking-tight md:text-5xl">{item.title}</h1>
          )}
          {item.original_title && item.original_title !== item.title && (
            <p className="-mt-2 text-sm text-muted">{item.original_title}</p>
          )}
          <div className="flex flex-wrap items-center gap-x-3 gap-y-2 text-sm text-fg/85">
            {item.community_rating != null && (
              <span className="inline-flex items-center gap-1 font-semibold">
                <Star className="size-4 fill-amber-400 text-amber-400" aria-hidden="true" />
                {item.community_rating.toFixed(1)}
              </span>
            )}
            {meta.map((part, i) => (
              <span key={i} className="after:ml-3 after:text-subtle after:content-['·'] last:after:content-none">
                {part}
              </span>
            ))}
            {badges.map((badge) => (
              <span key={badge} className="rounded border border-fg/30 px-1.5 py-0.5 text-[11px] font-bold tracking-wide">
                {badge}
              </span>
            ))}
          </div>
          {item.tagline && <p className="text-lg font-semibold text-fg/90 italic">{item.tagline}</p>}
          {item.overview && <p className="max-w-2xl text-base leading-relaxed text-fg/80" data-testid="item-overview">{item.overview}</p>}
          {(item.genres.length > 0 || directors.length > 0) && (
            <div className="flex flex-wrap gap-2 text-sm">
              {item.genres.map((genre) => (
                <Link
                  key={genre}
                  to={item.kind === 'series' ? '/shows' : '/movies'}
                  search={{ genre }}
                  className="rounded-full bg-white/10 px-3 py-1 font-semibold backdrop-blur hover:bg-white/20"
                >
                  {genre}
                </Link>
              ))}
              {directors.length > 0 && <span className="self-center pl-1 text-muted">· {directors.slice(0, 2).join(', ')}</span>}
            </div>
          )}

          <div className="flex flex-wrap items-center gap-3 pt-2">
            <PlayActions item={item} />
            <IconToggle
              active={item.played}
              label={item.played ? 'Als ungesehen markieren' : 'Als gesehen markieren'}
              onClick={() => setPlayed.mutate({ id: item.id, played: !item.played })}
            >
              <Check className="size-5" strokeWidth={item.played ? 3 : 2} />
            </IconToggle>
            <IconToggle
              active={item.favorite}
              label={item.favorite ? 'Aus Favoriten entfernen' : 'Zu Favoriten'}
              onClick={() => setFavorite.mutate({ id: item.id, favorite: !item.favorite })}
            >
              <Heart className={clsx('size-5', item.favorite && 'fill-current')} />
            </IconToggle>
            <AddToPlaylistButton item={item} />
            {session?.user.is_admin && (item.kind === 'movie' || item.kind === 'series') && (
              <>
                <IconToggle active={false} label="Zur Sammlung hinzufügen" onClick={() => setCollecting(true)}>
                  <Layers className="size-5" />
                </IconToggle>
                <IconToggle active={false} label="Identifizieren" onClick={() => setIdentifying(true)}>
                  <Search className="size-5" />
                </IconToggle>
                <IconToggle active={false} label="Metadaten aktualisieren" onClick={() => refresh.mutate(item.id)}>
                  <RefreshCw className={clsx('size-5', refresh.isPending && 'animate-spin')} />
                </IconToggle>
              </>
            )}
            {session?.user.is_admin && (item.kind === 'movie' || item.kind === 'episode') && item.files.length > 0 && (
              <IconToggle active={false} label="Untertitel suchen" onClick={() => setSearchingSubtitles(true)}>
                <Captions className="size-5" />
              </IconToggle>
            )}
          </div>
        </div>
      </div>
      {searchingSubtitles && <SubtitleSearchDialog item={item} onClose={() => setSearchingSubtitles(false)} />}
      {identifying && <IdentifyDialog itemId={item.id} title={item.title} onClose={() => setIdentifying(false)} />}
      {collecting && <AddToCollectionDialog item={item} onClose={() => setCollecting(false)} />}
    </section>
  )
}

function IconToggle({ active, label, onClick, children }: { active: boolean; label: string; onClick: () => void; children: React.ReactNode }) {
  return (
    <button
      type="button"
      aria-label={label}
      aria-pressed={active}
      title={label}
      onClick={onClick}
      className={clsx(
        'flex size-11 items-center justify-center rounded-full backdrop-blur transition-colors',
        active ? 'bg-accent text-accent-fg hover:bg-accent-hover' : 'bg-white/15 hover:bg-white/25',
      )}
    >
      {children}
    </button>
  )
}

function Seasons({ series, initial }: { series: ItemDetail; initial?: number }) {
  const seasons = useItems({ parent_id: series.id, limit: 100 })
  const list = seasons.data?.items ?? []
  const [chosen, setChosen] = useState<number | null>(null)
  const active = list.find((s) => s.index_number === (chosen ?? initial)) ?? list[0]
  const episodes = useItems({ parent_id: active?.id, limit: 200 }, !!active)

  if (list.length === 0) return null
  return (
    <section className="mb-10 px-6 md:px-10" aria-label="Staffeln">
      <div role="tablist" aria-label="Staffel wählen" className="mb-5 flex gap-1 overflow-x-auto">
        {list.map((s) => (
          <button
            key={s.id}
            role="tab"
            aria-selected={s.id === active?.id}
            onClick={() => setChosen(s.index_number ?? null)}
            className={clsx(
              'shrink-0 rounded-full px-4 py-2 text-sm font-semibold transition-colors',
              s.id === active?.id ? 'bg-fg text-canvas' : 'text-muted hover:bg-surface-2 hover:text-fg',
            )}
          >
            {s.title}
          </button>
        ))}
      </div>
      <ol className="space-y-3" data-testid="episode-list">
        {episodes.data?.items.map((episode) => (
          <EpisodeRow key={episode.id} episode={episode} />
        ))}
      </ol>
    </section>
  )
}

function EpisodeRow({ episode }: { episode: ItemCard }) {
  const runtime = formatRuntime(episode.runtime_ms)
  return (
    <li>
      <Link
        to="/item/$id"
        params={{ id: String(episode.id) }}
        className="group flex gap-4 rounded-[var(--radius-card)] p-2 hover:bg-surface-2"
      >
        <div className="relative w-40 shrink-0 overflow-hidden rounded-lg md:w-56">
          <BlurImage src={episode.poster_url} blurhash={episode.poster_blurhash} alt="" className="aspect-video" />
          {episode.played && (
            <span className="absolute top-1.5 right-1.5 flex size-5 items-center justify-center rounded-full bg-accent text-accent-fg">
              <Check className="size-3.5" strokeWidth={3} aria-label="Gesehen" />
            </span>
          )}
          {episode.position_ms > 0 && episode.runtime_ms && (
            <div className="absolute inset-x-1.5 bottom-1.5 h-1 overflow-hidden rounded-full bg-black/60">
              <div className="h-full bg-accent" style={{ width: `${Math.min(100, Math.round((episode.position_ms / episode.runtime_ms) * 100))}%` }} />
            </div>
          )}
        </div>
        <div className="min-w-0 py-1">
          <p className="text-xs font-semibold text-subtle">
            Folge {episode.index_number}
            {episode.index_end && episode.index_end > (episode.index_number ?? 0) ? `–${episode.index_end}` : ''}
            {runtime && ` · ${runtime}`}
            {episode.premiere_date && ` · ${new Date(episode.premiere_date).toLocaleDateString('de')}`}
          </p>
          <p className="truncate font-bold group-hover:text-white">{episode.title}</p>
          {episode.overview && <p className="mt-1 line-clamp-2 text-sm text-muted">{episode.overview}</p>}
        </div>
      </Link>
    </li>
  )
}

function TechnicalInfo({ item }: { item: ItemDetail }) {
  const mains = item.files.filter((f) => f.role === 'main')
  const extras = item.files.filter((f) => f.role === 'extra')
  if (mains.length === 0) return null
  return (
    <section className="mb-10 grid gap-4 px-6 md:grid-cols-2 md:px-10" aria-label="Technische Details">
      {mains.map((file) => (
        <div key={file.id} className="rounded-[var(--radius-card)] border border-line/60 bg-surface/60 p-4 text-sm">
          <p className="mb-2 truncate font-bold" title={file.file_name}>
            {mains.length > 1 && file.part ? `Teil ${file.part}: ` : ''}
            {file.file_name}
          </p>
          <dl className="grid grid-cols-[6rem_1fr] gap-y-1 text-muted">
            {file.video && (
              <>
                <dt className="text-subtle">Video</dt>
                <dd>
                  {file.video.codec?.toUpperCase()} {file.video.width && `${file.video.width}×${file.video.height}`}
                  {file.video.hdr && file.video.hdr !== 'sdr' && ` · ${file.video.hdr.toUpperCase()}`}
                </dd>
              </>
            )}
            {file.audio.length > 0 && (
              <>
                <dt className="text-subtle">Audio</dt>
                <dd>
                  {file.audio
                    .map((a) => `${languageName(a.language)} (${a.codec?.toUpperCase() ?? '?'}${a.channels ? ` ${a.channels}ch` : ''})`)
                    .join(', ')}
                </dd>
              </>
            )}
            {file.subtitles.length > 0 && (
              <>
                <dt className="text-subtle">Untertitel</dt>
                <dd>
                  {file.subtitles
                    .map((s) => `${languageName(s.language)}${s.is_forced ? ' (erzwungen)' : ''}${s.is_sdh ? ' (SDH)' : ''}`)
                    .join(', ')}
                </dd>
              </>
            )}
            {file.chapters.length > 0 && (
              <>
                <dt className="text-subtle">Kapitel</dt>
                <dd>{file.chapters.length}</dd>
              </>
            )}
          </dl>
        </div>
      ))}
      {extras.length > 0 && (
        <div className="rounded-[var(--radius-card)] border border-line/60 bg-surface/60 p-4 text-sm">
          <p className="mb-2 font-bold">Extras</p>
          <ul className="space-y-1 text-muted">
            {extras.map((e) => (
              <li key={e.id} className="truncate">
                {e.file_name}
              </li>
            ))}
          </ul>
        </div>
      )}
    </section>
  )
}

function MoreLikeThis({ item }: { item: ItemDetail }) {
  const genre = item.genres[0]
  const similar = useItems({ kind: item.kind, genre, sort: 'rating', order: 'desc', limit: 20 }, !!genre)
  const items = similar.data?.items.filter((i) => i.id !== item.id)
  return <Row title="Mehr wie das" items={items} />
}
