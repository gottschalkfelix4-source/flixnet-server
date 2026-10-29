import { Link, useParams, useRouter } from '@tanstack/react-router'
import { ArrowLeft, TriangleAlert } from 'lucide-react'
import { useState } from 'react'
import { useItem } from '@/api/queries/items'
import { REASON_LABELS, usePlaybackInfo } from '@/api/queries/playback'
import { episodeLabel } from '@/lib/media'

/**
 * Full-screen player. Plays the original file when the decision engine says Direct
 * Play; remux/transcode streams, track selection and custom controls follow in later
 * milestones.
 */
export function PlayerPage() {
  const { id } = useParams({ from: '/play/$id' })
  const itemId = Number(id)
  const item = useItem(itemId)
  const playback = usePlaybackInfo(itemId)
  const router = useRouter()
  const [part, setPart] = useState(0)
  const [failed, setFailed] = useState(false)

  const sources = playback.data?.sources ?? []
  const source = sources[part]
  const direct = source?.plan.method === 'direct_play'
  // A file the engine judged playable can still fail (e.g. a broken file or a wrong guess).
  const blocked = failed || playback.isError || (!!source && !direct)
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
      </header>

      {source && direct && !failed && (
        <video
          key={source.file_id}
          src={source.direct_url}
          controls
          autoPlay
          playsInline
          className="size-full"
          data-testid="player-video"
          onEnded={() => part + 1 < sources.length && setPart(part + 1)}
          onError={() => setFailed(true)}
        />
      )}

      {blocked && (
        <div className="flex size-full flex-col items-center justify-center gap-3 p-6 text-center" role="alert">
          <TriangleAlert className="size-10 text-amber-400" aria-hidden="true" />
          <p className="text-lg font-bold">Dieses Video kann dein Browser nicht direkt abspielen.</p>
          {playback.isError ? (
            <p className="max-w-md text-sm text-white/70">Für diesen Eintrag gibt es keine abspielbare Datei.</p>
          ) : (
            <>
              {source && !direct && (
                <ul className="max-w-md space-y-1 text-sm text-white/70" data-testid="playback-reasons">
                  {source.plan.reasons.map((reason) => (
                    <li key={reason}>{REASON_LABELS[reason]}</li>
                  ))}
                </ul>
              )}
              <p className="max-w-md text-sm text-white/50">
                {source && !direct
                  ? `Nötig wäre: ${source.plan.method === 'remux' ? 'Umpacken (Remux)' : 'Umwandeln (Transcoding)'} – kommt mit den nächsten Meilensteinen.`
                  : 'Die Datei ließ sich nicht abspielen.'}
              </p>
            </>
          )}
          <Link to="/item/$id" params={{ id }} className="mt-2 rounded-full bg-white/15 px-5 py-2 text-sm font-semibold hover:bg-white/25">
            Zur Detailseite
          </Link>
        </div>
      )}
    </div>
  )
}
