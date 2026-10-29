import { Link, useParams, useRouter } from '@tanstack/react-router'
import { ArrowLeft, TriangleAlert } from 'lucide-react'
import { useRef, useState } from 'react'
import { useItem } from '@/api/queries/items'
import { REASON_LABELS, usePlaybackInfo } from '@/api/queries/playback'
import { episodeLabel } from '@/lib/media'
import { useVideoSource } from '@/lib/useVideoSource'

/**
 * Full-screen player: the original file when the decision engine says Direct Play,
 * otherwise an HLS transcode. Track selection and custom controls follow in later
 * milestones.
 */
export function PlayerPage() {
  const { id } = useParams({ from: '/play/$id' })
  const itemId = Number(id)
  const item = useItem(itemId)
  const playback = usePlaybackInfo(itemId)
  const router = useRouter()
  const video = useRef<HTMLVideoElement>(null)
  const [part, setPart] = useState(0)
  const [failed, setFailed] = useState(false)

  const sources = playback.data?.sources ?? []
  const source = sources[part]
  const streamError = useVideoSource(video, source)
  const error = playback.isError
    ? 'Für diesen Eintrag gibt es keine abspielbare Datei.'
    : (streamError ?? (failed ? 'Die Datei ließ sich nicht abspielen.' : null))

  const title = item.data
    ? [item.data.series_title, episodeLabel(item.data), item.data.title].filter(Boolean).join(' · ')
    : ''
  const back = () => (router.history.length > 1 ? router.history.back() : void router.navigate({ to: '/item/$id', params: { id } }))

  return (
    <div className="fixed inset-0 bg-black text-white">
      <header className="absolute inset-x-0 top-0 z-10 flex items-center gap-3 bg-gradient-to-b from-black/80 to-transparent p-4">
        <button
          type="button"
          onClick={back}
          aria-label="Zurück"
          className="flex size-10 items-center justify-center rounded-full hover:bg-white/15"
        >
          <ArrowLeft className="size-6" />
        </button>
        <h1 className="truncate text-lg font-bold" data-testid="player-title">
          {title}
        </h1>
        {sources.length > 1 && (
          <span className="text-sm text-white/70">
            Teil {part + 1} von {sources.length}
          </span>
        )}
        {source && (
          <span className="ml-auto rounded bg-white/15 px-2 py-0.5 text-xs font-semibold" data-testid="play-method">
            {source.plan.method === 'direct_play' ? 'Direct Play' : source.plan.method === 'remux' ? 'Remux' : 'Transcode'}
          </span>
        )}
      </header>

      <video
        ref={video}
        controls
        autoPlay
        playsInline
        hidden={!!error}
        className="size-full"
        data-testid="player-video"
        onEnded={() => part + 1 < sources.length && setPart(part + 1)}
        onError={() => setFailed(true)}
      />

      {error && (
        <div className="flex size-full flex-col items-center justify-center gap-3 p-6 text-center" role="alert">
          <TriangleAlert className="size-10 text-amber-400" aria-hidden="true" />
          <p className="text-lg font-bold">{error}</p>
          {source && source.plan.reasons.length > 0 && (
            <ul className="max-w-md space-y-1 text-sm text-white/70" data-testid="playback-reasons">
              {source.plan.reasons.map((reason) => (
                <li key={reason}>{REASON_LABELS[reason]}</li>
              ))}
            </ul>
          )}
          <Link to="/item/$id" params={{ id }} className="mt-2 rounded-full bg-white/15 px-5 py-2 text-sm font-semibold hover:bg-white/25">
            Zur Detailseite
          </Link>
        </div>
      )}
    </div>
  )
}
