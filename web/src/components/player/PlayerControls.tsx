import {
  Maximize,
  Minimize,
  Pause,
  PictureInPicture2,
  Play,
  RotateCcw,
  RotateCw,
  SkipForward,
  Volume1,
  Volume2,
  VolumeX,
} from 'lucide-react'
import clsx from 'clsx'
import type { ReactNode } from 'react'
import type { Chapter, Trickplay } from '@/api/queries/player'
import { SeekBar } from '@/components/player/SeekBar'
import { formatTimecode } from '@/lib/format'
import type { MediaState } from '@/lib/useMediaState'

interface PlayerControlsProps {
  media: MediaState
  chapters: Chapter[]
  trickplay: Trickplay | null | undefined
  fullscreen: boolean
  /** Picture-in-picture is offered only where the browser has it. */
  pipAvailable: boolean
  /** Title of the next episode, when there is one. */
  nextLabel: string | null
  onTogglePlay: () => void
  onSeek: (seconds: number) => void
  onSkip: (seconds: number) => void
  onToggleMute: () => void
  onVolume: (volume: number) => void
  onToggleFullscreen: () => void
  onTogglePip: () => void
  onNext: () => void
  /** The settings sheet (gear button). */
  settings: ReactNode
  /** More buttons before the settings (watch together, cast). */
  extra?: ReactNode
}

/** The control bar: seek bar on top, buttons below. */
export function PlayerControls(props: PlayerControlsProps) {
  const { media } = props
  const volume = media.muted ? 0 : media.volume
  const VolumeIcon = volume === 0 ? VolumeX : volume < 0.5 ? Volume1 : Volume2

  return (
    <div className="pointer-events-auto px-4 pb-3 sm:px-8 sm:pb-5" data-testid="player-controls">
      <SeekBar
        currentTime={media.currentTime}
        duration={media.duration}
        bufferedEnd={media.bufferedEnd}
        chapters={props.chapters}
        trickplay={props.trickplay}
        onSeek={props.onSeek}
      />
      <div className="mt-1 flex items-center gap-0.5 sm:gap-2">
        <IconButton label={media.paused ? 'Abspielen' : 'Pause'} shortcut="K" onClick={props.onTogglePlay}>
          {media.paused ? <Play className="size-7 fill-current" /> : <Pause className="size-7 fill-current" />}
        </IconButton>
        <IconButton label="10 Sekunden rückwärts" shortcut="J" wide onClick={() => props.onSkip(-10)}>
          <RotateCcw className="size-6" />
        </IconButton>
        <IconButton label="10 Sekunden vorwärts" shortcut="L" wide onClick={() => props.onSkip(10)}>
          <RotateCw className="size-6" />
        </IconButton>
        <div className="group/volume flex items-center">
          <IconButton label={media.muted ? 'Ton an' : 'Ton aus'} shortcut="M" onClick={props.onToggleMute}>
            <VolumeIcon className="size-6" />
          </IconButton>
          <input
            type="range"
            min={0}
            max={1}
            step={0.05}
            value={volume}
            onChange={(event) => props.onVolume(Number(event.target.value))}
            aria-label="Lautstärke"
            aria-valuetext={`${Math.round(volume * 100)} %`}
            className="hidden w-0 accent-[var(--color-accent)] opacity-0 transition-all duration-200 group-focus-within/volume:mx-1 group-focus-within/volume:w-24 group-focus-within/volume:opacity-100 group-hover/volume:mx-1 group-hover/volume:w-24 group-hover/volume:opacity-100 sm:block"
          />
        </div>
        <span className="ml-1 text-sm font-semibold whitespace-nowrap tabular-nums sm:ml-2" data-testid="player-time">
          {formatTimecode(media.currentTime * 1000)}
          <span className="text-white/60"> / {formatTimecode(media.duration * 1000)}</span>
        </span>

        <div className="ml-auto flex items-center gap-0.5 sm:gap-2">
          {props.nextLabel && (
            <IconButton label={`Nächste Folge: ${props.nextLabel}`} shortcut="N" onClick={props.onNext}>
              <SkipForward className="size-6" />
            </IconButton>
          )}
          {props.extra}
          {props.settings}
          {props.pipAvailable && (
            <IconButton label="Bild-in-Bild" wide onClick={props.onTogglePip}>
              <PictureInPicture2 className="size-6" />
            </IconButton>
          )}
          <IconButton
            label={props.fullscreen ? 'Vollbild beenden' : 'Vollbild'}
            shortcut="F"
            onClick={props.onToggleFullscreen}
          >
            {props.fullscreen ? <Minimize className="size-6" /> : <Maximize className="size-6" />}
          </IconButton>
        </div>
      </div>
    </div>
  )
}

function IconButton({
  label,
  shortcut,
  wide,
  onClick,
  children,
}: {
  label: string
  shortcut?: string
  /** Only on wider screens (phones keep the essentials). */
  wide?: boolean
  onClick: () => void
  children: ReactNode
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={label}
      aria-keyshortcuts={shortcut}
      title={shortcut ? `${label} (${shortcut})` : label}
      className={clsx(
        'size-11 shrink-0 items-center justify-center rounded-full hover:bg-white/15 [&>svg]:pointer-events-none',
        wide ? 'hidden sm:flex' : 'flex',
      )}
    >
      {children}
    </button>
  )
}
