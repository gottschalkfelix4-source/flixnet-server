import clsx from 'clsx'
import { Check, Settings } from 'lucide-react'
import { type ReactNode, useEffect, useRef, useState } from 'react'
import type { AudioTrack, PlaybackSource, SubtitleTrack } from '@/api/queries/playback'
import { audioDetail, audioLabel, subtitleAvailable, subtitleDetail, subtitleLabel } from '@/lib/tracks'

interface TrackMenuProps {
  source: PlaybackSource
  /** Effective subtitle track (`null` = off). */
  subtitle: number | null
  onAudio: (track: AudioTrack) => void
  onSubtitle: (track: SubtitleTrack | null) => void
}

/** Gear button with the audio and subtitle tracks of the current source. */
export function TrackMenu({ source, subtitle, onAudio, onSubtitle }: TrackMenuProps) {
  const [open, setOpen] = useState(false)
  const root = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!open) return
    const close = (event: Event) => {
      if (event instanceof KeyboardEvent ? event.key === 'Escape' : !root.current?.contains(event.target as Node)) {
        setOpen(false)
      }
    }
    document.addEventListener('pointerdown', close)
    document.addEventListener('keydown', close)
    return () => {
      document.removeEventListener('pointerdown', close)
      document.removeEventListener('keydown', close)
    }
  }, [open])

  const audio = source.plan.audio?.stream_index
  const pick = (action: () => void) => () => {
    setOpen(false)
    action()
  }

  return (
    <div ref={root} className="relative">
      <button
        type="button"
        onClick={() => setOpen(!open)}
        aria-label="Wiedergabe-Einstellungen"
        aria-haspopup="menu"
        aria-expanded={open}
        title="Ton und Untertitel"
        className="flex size-10 items-center justify-center rounded-full hover:bg-white/15"
      >
        <Settings className="size-5.5" />
      </button>
      {open && (
        <div
          role="menu"
          aria-label="Wiedergabe-Einstellungen"
          className="absolute top-12 right-0 z-20 max-h-[70vh] w-80 overflow-y-auto rounded-xl bg-neutral-900/95 p-2 shadow-2xl ring-1 ring-white/10"
        >
          <Group label="Tonspur">
            {source.audio_tracks.length === 0 && <p className="px-3 py-2 text-sm text-white/60">Keine Tonspur</p>}
            {source.audio_tracks.map((track) => (
              <Item
                key={track.index}
                checked={track.index === audio}
                label={audioLabel(track)}
                detail={audioDetail(track)}
                onSelect={pick(() => track.index !== audio && onAudio(track))}
              />
            ))}
          </Group>
          <Group label="Untertitel">
            <Item checked={subtitle === null} label="Aus" onSelect={pick(() => subtitle !== null && onSubtitle(null))} />
            {source.subtitle_tracks.map((track) => (
              <Item
                key={track.index}
                checked={track.index === subtitle}
                disabled={!subtitleAvailable(track)}
                label={subtitleLabel(track)}
                detail={subtitleDetail(track)}
                onSelect={pick(() => track.index !== subtitle && onSubtitle(track))}
              />
            ))}
          </Group>
        </div>
      )}
    </div>
  )
}

function Group({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div role="group" aria-label={label} className="py-1">
      <p className="px-3 pt-1 pb-1.5 text-xs font-bold tracking-wide text-white/50 uppercase" aria-hidden="true">
        {label}
      </p>
      {children}
    </div>
  )
}

interface ItemProps {
  checked: boolean
  disabled?: boolean
  label: string
  detail?: string
  onSelect: () => void
}

function Item({ checked, disabled, label, detail, onSelect }: ItemProps) {
  return (
    <button
      type="button"
      role="menuitemradio"
      aria-checked={checked}
      disabled={disabled}
      onClick={onSelect}
      className={clsx(
        'flex w-full items-start gap-2 rounded-lg px-3 py-2 text-left text-sm',
        disabled ? 'cursor-not-allowed text-white/35' : 'hover:bg-white/10',
      )}
    >
      <Check className={clsx('mt-0.5 size-4 shrink-0', checked ? 'text-accent' : 'invisible')} aria-hidden="true" />
      <span className="min-w-0">
        <span className="block font-semibold">{label}</span>
        {detail && <span className="block truncate text-xs text-white/60">{detail}</span>}
      </span>
    </button>
  )
}
