import { Pause, Play, Radio, RotateCcw, RotateCw, Volume2, VolumeX } from 'lucide-react'
import { type RefObject, useEffect, useState } from 'react'
import { formatClock } from '@/api/queries/livetv'
import { formatTimecode } from '@/lib/format'
import { LIVE_EDGE_SECS } from '@/lib/useLiveSource'

interface Position {
  start: number
  end: number
  current: number
  paused: boolean
  muted: boolean
}

function read(video: HTMLVideoElement): Position | null {
  const seekable = video.seekable
  if (seekable.length === 0) return null
  return {
    start: seekable.start(0),
    end: seekable.end(seekable.length - 1),
    current: video.currentTime,
    paused: video.paused,
    muted: video.muted,
  }
}

/** Seconds behind the newest segment that still count as "live". */
const LIVE_SLACK = LIVE_EDGE_SECS + 6

/**
 * Timeshift controls of the live player: pause/play, 30 s back and forward, a
 * timeline over the server's buffer, how far behind live the picture is, and "Live"
 * to jump back to the edge.
 */
export function TimeshiftControls({
  video,
  onSeek,
  onTogglePlay,
  onToggleMute,
  onLive,
  playingDate,
}: {
  video: RefObject<HTMLVideoElement | null>
  onSeek: (seconds: number) => void
  onTogglePlay: () => void
  onToggleMute: () => void
  onLive: () => void
  playingDate: (element: HTMLVideoElement) => Date | null
}) {
  const [position, setPosition] = useState<Position | null>(null)
  const [clock, setClock] = useState<Date | null>(null)

  useEffect(() => {
    const element = video.current
    if (!element) return
    const update = () => {
      setPosition(read(element))
      setClock(playingDate(element))
    }
    const events = ['timeupdate', 'pause', 'play', 'progress', 'durationchange', 'volumechange', 'seeking']
    for (const e of events) element.addEventListener(e, update)
    // The buffer grows while paused, too.
    const timer = window.setInterval(update, 1000)
    return () => {
      for (const e of events) element.removeEventListener(e, update)
      window.clearInterval(timer)
    }
  }, [video, playingDate])

  const behind = position ? Math.max(0, position.end - position.current) : 0
  const live = position !== null && !position.paused && behind <= LIVE_SLACK
  const seek = (to: number) => {
    if (position) onSeek(Math.min(position.end - 1, Math.max(position.start, to)))
  }

  return (
    <div
      className="absolute inset-x-0 bottom-0 z-10 flex flex-col gap-2 bg-gradient-to-t from-black/85 to-transparent px-4 pt-10 pb-4"
      data-testid="timeshift-controls"
    >
      <input
        type="range"
        aria-label="Zeitversatz"
        className="w-full accent-[var(--color-accent)]"
        min={position?.start ?? 0}
        max={position?.end ?? 1}
        step={1}
        value={position?.current ?? 0}
        disabled={!position}
        onChange={(event) => seek(Number(event.target.value))}
      />
      <div className="flex items-center gap-1">
        <button
          type="button"
          aria-label={position?.paused ? 'Abspielen' : 'Pause'}
          onClick={onTogglePlay}
          className="flex size-10 items-center justify-center rounded-full hover:bg-white/15"
        >
          {position?.paused ? <Play className="size-5 fill-current" /> : <Pause className="size-5 fill-current" />}
        </button>
        <button
          type="button"
          aria-label="30 Sekunden zurück"
          onClick={() => position && seek(position.current - 30)}
          className="flex size-10 items-center justify-center rounded-full hover:bg-white/15"
        >
          <RotateCcw className="size-5" />
        </button>
        <button
          type="button"
          aria-label="30 Sekunden vor"
          onClick={() => position && seek(position.current + 30)}
          disabled={live}
          className="flex size-10 items-center justify-center rounded-full hover:bg-white/15 disabled:opacity-40"
        >
          <RotateCw className="size-5" />
        </button>
        <button
          type="button"
          aria-label={position?.muted ? 'Ton an' : 'Stumm'}
          onClick={onToggleMute}
          className="flex size-10 items-center justify-center rounded-full hover:bg-white/15"
        >
          {position?.muted ? <VolumeX className="size-5" /> : <Volume2 className="size-5" />}
        </button>
        <span className="ml-2 text-sm tabular-nums text-white/80" data-testid="timeshift-offset">
          {live ? 'Live' : `−${formatTimecode(behind * 1000)}`}
        </span>
        {clock && !live && <span className="text-sm text-white/60 tabular-nums">· {formatClock(clock.getTime())} Uhr</span>}
        <span className="ml-auto text-xs text-white/50">
          {position ? `Puffer ${formatTimecode((position.end - position.start) * 1000)}` : ''}
        </span>
        <button
          type="button"
          onClick={onLive}
          aria-pressed={live}
          data-testid="live-edge"
          className={`ml-3 inline-flex h-9 items-center gap-1.5 rounded-full px-4 text-sm font-bold ${
            live ? 'bg-red-600 text-white' : 'bg-white/15 text-white hover:bg-white/25'
          }`}
        >
          <Radio className="size-4" aria-hidden="true" />
          Live
        </button>
      </div>
    </div>
  )
}
