import { Link } from '@tanstack/react-router'
import { Play, RotateCcw } from 'lucide-react'
import type { ItemCard, ItemDetail } from '@/api/queries/items'
import { useSeriesNextUp } from '@/api/queries/playstate'
import { episodeLabel, formatRuntime } from '@/lib/media'

const primary =
  'relative inline-flex h-11 items-center gap-2 overflow-hidden rounded-full bg-fg px-6 text-sm font-bold text-canvas hover:bg-white'

/**
 * Play button of a detail page: "Fortsetzen" with progress when there is a resume
 * point (plus "Von vorne"), otherwise "Abspielen". Series play their next episode.
 */
export function PlayActions({ item }: { item: ItemDetail }) {
  const isSeries = item.kind === 'series'
  const nextUp = useSeriesNextUp(item.id, isSeries)

  if (item.kind === 'movie' || item.kind === 'episode') return <PlayLink target={item} />
  if (isSeries && nextUp.data) return <PlayLink target={nextUp.data} showEpisode />
  return (
    <button type="button" disabled className={`${primary} disabled:opacity-60`}>
      <Play className="size-5 fill-current" aria-hidden="true" />
      Abspielen
    </button>
  )
}

function PlayLink({ target, showEpisode = false }: { target: ItemCard; showEpisode?: boolean }) {
  const resume = target.position_ms > 0
  const label = showEpisode ? episodeLabel(target) : null
  const runtime = target.runtime_ms ?? 0
  const progress = resume && runtime > 0 ? Math.min(1, target.position_ms / runtime) : null
  const remaining = progress !== null ? formatRuntime(runtime - target.position_ms) : null

  return (
    <>
      <Link to="/play/$id" params={{ id: String(target.id) }} className={primary} data-testid="play-button">
        <Play className="size-5 fill-current" aria-hidden="true" />
        {resume ? 'Fortsetzen' : 'Abspielen'}
        {label && <span className="font-semibold opacity-60">{label}</span>}
        {progress !== null && (
          <span aria-hidden="true" className="absolute inset-x-5 bottom-1.5 h-1 overflow-hidden rounded-full bg-canvas/20">
            <span className="block h-full bg-accent" style={{ width: `${Math.round(progress * 100)}%` }} />
          </span>
        )}
      </Link>
      {resume && (
        <Link
          to="/play/$id"
          params={{ id: String(target.id) }}
          search={{ start: 0 }}
          className="inline-flex h-11 items-center gap-2 rounded-full bg-white/15 px-5 text-sm font-bold backdrop-blur hover:bg-white/25"
        >
          <RotateCcw className="size-4" aria-hidden="true" />
          Von vorne
        </Link>
      )}
      {remaining && <span className="text-sm text-muted" data-testid="remaining-time">Noch {remaining}</span>}
    </>
  )
}
