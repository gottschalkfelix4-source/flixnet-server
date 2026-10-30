import { Cast, Pause, Play, X } from 'lucide-react'
import type { RemoteState } from '@/lib/useCast'
import { formatTimecode } from '@/lib/format'

/** Cast button for the control bar (only shown when a cast device is around). */
export function CastButton({ active, onClick }: { active: boolean; onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={active ? 'Übertragung läuft' : 'Auf Gerät übertragen'}
      title="Chromecast"
      className="flex size-11 shrink-0 items-center justify-center rounded-full hover:bg-white/15 [&>svg]:pointer-events-none"
      data-testid="cast-button"
    >
      <Cast className={active ? 'size-6 text-[var(--color-accent)]' : 'size-6'} />
    </button>
  )
}

/** Remote control while the item plays on a cast device. */
export function CastOverlay({
  remote,
  title,
  onPlayPause,
  onSeek,
  onStop,
}: {
  remote: RemoteState
  title: string
  onPlayPause: () => void
  onSeek: (seconds: number) => void
  onStop: () => void
}) {
  const duration = Number.isFinite(remote.duration) ? remote.duration : 0
  return (
    <div
      className="absolute inset-0 z-40 flex flex-col items-center justify-center gap-6 bg-black/90 p-6 text-center text-white"
      data-testid="cast-overlay"
    >
      <Cast className="size-12 text-[var(--color-accent)]" aria-hidden="true" />
      <div>
        <p className="text-lg font-bold">{title}</p>
        <p className="text-sm text-white/70" data-testid="cast-device">
          Wird auf {remote.device} abgespielt
        </p>
      </div>
      <div className="flex w-full max-w-lg items-center gap-3">
        <button
          type="button"
          onClick={onPlayPause}
          aria-label={remote.paused ? 'Abspielen' : 'Pause'}
          className="flex size-12 shrink-0 items-center justify-center rounded-full bg-white/15 hover:bg-white/25"
          data-testid="cast-play-pause"
        >
          {remote.paused ? <Play className="size-6 fill-current" /> : <Pause className="size-6 fill-current" />}
        </button>
        <span className="text-xs tabular-nums">{formatTimecode(remote.currentTime * 1000)}</span>
        <input
          type="range"
          min={0}
          max={Math.max(1, duration)}
          step={1}
          value={Math.min(remote.currentTime, duration || remote.currentTime)}
          onChange={(e) => onSeek(Number(e.target.value))}
          aria-label="Position auf dem Gerät"
          className="min-w-0 flex-1 accent-[var(--color-accent)]"
        />
        <span className="text-xs tabular-nums">{formatTimecode(duration * 1000)}</span>
      </div>
      <button
        type="button"
        onClick={onStop}
        className="flex items-center gap-2 rounded-full bg-white/10 px-5 py-2 text-sm font-semibold hover:bg-white/20"
      >
        <X className="size-4" aria-hidden="true" /> Übertragung beenden
      </button>
    </div>
  )
}
