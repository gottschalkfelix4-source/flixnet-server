import { useParams, useRouter } from '@tanstack/react-router'
import { ArrowLeft, LoaderCircle, TriangleAlert } from 'lucide-react'
import { useRef } from 'react'
import { ApiError } from '@/api/client'
import { formatSlot, progress, useChannels, useWatchChannel } from '@/api/queries/livetv'
import { REASON_LABELS } from '@/api/queries/playback'
import { TimeshiftControls } from '@/components/livetv/TimeshiftControls'
import { useLiveSource } from '@/lib/useLiveSource'
import { useNow } from '@/lib/useNow'

/**
 * Full-screen live player: asks the server for a viewer session of the channel (it
 * joins or starts the shared stream) and plays it from the live edge, with timeshift
 * controls over the server's buffer (pause, rewind, back to live).
 */
export function LivePlayerPage() {
  const { channelId } = useParams({ from: '/live/$channelId' })
  const id = Number(channelId)
  const router = useRouter()
  const video = useRef<HTMLVideoElement>(null)
  const channels = useChannels()
  const channel = channels.data?.find((c) => c.id === id)
  const session = useWatchChannel(id)
  const live = useLiveSource(video, session.data)
  const now = useNow(15_000)

  const error = session.isError
    ? session.error instanceof ApiError
      ? session.error.message
      : 'Der Sender lässt sich gerade nicht abspielen.'
    : live.error
  const plan = session.data?.plan
  const method = plan
    ? plan.video?.action === 'transcode' || plan.audio?.action === 'transcode'
      ? 'Transcode'
      : 'Kopie'
    : null
  const current = channel?.now
  const seek = (seconds: number) => {
    const element = video.current
    if (element) element.currentTime = seconds
  }
  const togglePlay = () => {
    const element = video.current
    if (element?.paused) void element.play().catch(() => {})
    else element?.pause()
  }
  const toggleMute = () => {
    const element = video.current
    if (element) element.muted = !element.muted
  }
  const goLive = () => {
    const element = video.current
    if (!element) return
    const edge = live.liveEdge(element)
    if (edge !== null) element.currentTime = edge
    void element.play().catch(() => {})
  }
  const back = () => (router.history.length > 1 ? router.history.back() : void router.navigate({ to: '/live' }))

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
        <div className="min-w-0">
          <h1 className="truncate text-lg font-bold" data-testid="live-title">
            {channel ? [channel.number, channel.name].filter(Boolean).join(' · ') : 'Live TV'}
          </h1>
          {current && (
            <p className="truncate text-sm text-white/70">
              {current.title} · {formatSlot(current)}
            </p>
          )}
        </div>
        <span className="ml-auto flex items-center gap-2">
          <span className="rounded bg-red-600 px-2 py-0.5 text-xs font-bold tracking-wide">LIVE</span>
          {method && (
            <span
              className="rounded bg-white/15 px-2 py-0.5 text-xs font-semibold"
              data-testid="live-method"
              title={plan?.reasons.map((r) => REASON_LABELS[r]).join('\n')}
            >
              {method}
            </span>
          )}
        </span>
      </header>

      {current && (
        <div className="absolute inset-x-0 top-0 z-10 h-0.5 bg-white/10">
          <div className="h-full bg-accent" style={{ width: `${progress(current, now) * 100}%` }} />
        </div>
      )}

      <video
        ref={video}
        className="h-full w-full bg-black"
        autoPlay
        playsInline
        data-testid="live-video"
      />

      {session.data && !error && (
        <TimeshiftControls
          video={video}
          onSeek={seek}
          onTogglePlay={togglePlay}
          onToggleMute={toggleMute}
          onLive={goLive}
          playingDate={live.playingDate}
        />
      )}

      {session.isPending && (
        <div className="absolute inset-0 flex flex-col items-center justify-center gap-3 text-white/80">
          <LoaderCircle className="size-10 animate-spin" aria-hidden="true" />
          <p className="text-sm">Sender wird eingeschaltet …</p>
        </div>
      )}
      {error && (
        <div role="alert" className="absolute inset-0 flex flex-col items-center justify-center gap-3 bg-black/80 p-6 text-center">
          <TriangleAlert className="size-10 text-amber-400" aria-hidden="true" />
          <p className="max-w-md font-semibold">{error}</p>
          <button type="button" onClick={back} className="rounded-full bg-white/15 px-5 py-2 text-sm font-semibold hover:bg-white/25">
            Zurück
          </button>
        </div>
      )}
    </div>
  )
}
