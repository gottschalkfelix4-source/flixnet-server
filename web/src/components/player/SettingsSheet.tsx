import clsx from 'clsx'
import { Check, Settings } from 'lucide-react'
import { type ReactNode, useEffect, useRef } from 'react'
import type { AudioTrack, PlaybackSource, SubtitleTrack } from '@/api/queries/playback'
import type { Chapter } from '@/api/queries/player'
import { formatTimecode } from '@/lib/format'
import { audioDetail, audioLabel, subtitleAvailable, subtitleDetail, subtitleLabel } from '@/lib/tracks'
import { chapterAt, chapterTitle } from '@/lib/trickplay'

/** Quality limits offered in the player (bits/s; `undefined` = original). */
const QUALITY_OPTIONS: { value: number | undefined; label: string; detail?: string }[] = [
  { value: undefined, label: 'Original', detail: 'Ohne Begrenzung' },
  { value: 20_000_000, label: '20 Mbit/s', detail: 'bis 4K' },
  { value: 8_000_000, label: '8 Mbit/s', detail: 'bis 1080p' },
  { value: 4_000_000, label: '4 Mbit/s', detail: 'bis 720p' },
  { value: 2_000_000, label: '2 Mbit/s', detail: 'bis 480p' },
  { value: 1_000_000, label: '1 Mbit/s', detail: 'Mobile Daten' },
]

const SPEED_OPTIONS = [0.5, 0.75, 1, 1.25, 1.5, 2]

function speedLabel(rate: number): string {
  return rate === 1 ? 'Normal' : `${String(rate).replace('.', ',')}×`
}

interface SettingsSheetProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  source: PlaybackSource
  /** Effective subtitle track (`null` = off). */
  subtitle: number | null
  onAudio: (track: AudioTrack) => void
  onSubtitle: (track: SubtitleTrack | null) => void
  maxBitrate: number | undefined
  onQuality: (maxBitrate: number | undefined) => void
  speed: number
  onSpeed: (rate: number) => void
  chapters: Chapter[]
  /** Current position (ms), to mark the running chapter. */
  positionMs: number
  onChapter: (chapter: Chapter) => void
}

/**
 * Gear button with the playback settings: audio and subtitle tracks, quality limit,
 * speed and the chapter list (with images) to jump around. Opens upward from the
 * control bar; a choice closes it.
 */
export function SettingsSheet(props: SettingsSheetProps) {
  const { open, onOpenChange, source, subtitle, chapters } = props
  const root = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!open) return
    const close = (event: Event) => {
      if (event instanceof KeyboardEvent ? event.key === 'Escape' : !root.current?.contains(event.target as Node)) {
        onOpenChange(false)
      }
    }
    document.addEventListener('pointerdown', close)
    document.addEventListener('keydown', close)
    return () => {
      document.removeEventListener('pointerdown', close)
      document.removeEventListener('keydown', close)
    }
  }, [open, onOpenChange])

  const audio = source.plan.audio?.stream_index
  const pick = (action: () => void) => () => {
    onOpenChange(false)
    action()
  }
  const running = chapterAt(chapters, props.positionMs)

  return (
    <div ref={root} className="relative">
      <button
        type="button"
        onClick={() => onOpenChange(!open)}
        aria-label="Wiedergabe-Einstellungen"
        aria-haspopup="menu"
        aria-expanded={open}
        title="Einstellungen"
        className="flex size-11 items-center justify-center rounded-full hover:bg-white/15"
      >
        <Settings className={clsx('size-6 transition-transform', open && 'rotate-45')} aria-hidden="true" />
      </button>
      {open && (
        <div
          role="menu"
          aria-label="Wiedergabe-Einstellungen"
          className="absolute right-0 bottom-14 z-30 max-h-[min(70vh,36rem)] w-[min(22rem,calc(100vw-2rem))] overflow-y-auto overscroll-contain rounded-xl bg-neutral-900/95 p-2 shadow-2xl ring-1 ring-white/10 backdrop-blur"
        >
          <Group label="Tonspur">
            {source.audio_tracks.length === 0 && <p className="px-3 py-2 text-sm text-white/60">Keine Tonspur</p>}
            {source.audio_tracks.map((track) => (
              <Item
                key={track.index}
                checked={track.index === audio}
                label={audioLabel(track)}
                detail={audioDetail(track)}
                onSelect={pick(() => track.index !== audio && props.onAudio(track))}
              />
            ))}
          </Group>
          <Group label="Untertitel">
            <Item checked={subtitle === null} label="Aus" onSelect={pick(() => subtitle !== null && props.onSubtitle(null))} />
            {source.subtitle_tracks.map((track) => (
              <Item
                key={track.index}
                checked={track.index === subtitle}
                disabled={!subtitleAvailable(track)}
                label={subtitleLabel(track)}
                detail={subtitleDetail(track)}
                onSelect={pick(() => track.index !== subtitle && props.onSubtitle(track))}
              />
            ))}
          </Group>
          <Group label="Qualität">
            {QUALITY_OPTIONS.map((option) => (
              <Item
                key={option.label}
                checked={option.value === props.maxBitrate}
                label={option.label}
                detail={option.detail}
                onSelect={pick(() => option.value !== props.maxBitrate && props.onQuality(option.value))}
              />
            ))}
          </Group>
          <Group label="Geschwindigkeit">
            <div className="grid grid-cols-3 gap-1 px-1">
              {SPEED_OPTIONS.map((rate) => (
                <button
                  key={rate}
                  type="button"
                  role="menuitemradio"
                  aria-checked={rate === props.speed}
                  onClick={pick(() => props.onSpeed(rate))}
                  className={clsx(
                    'rounded-lg px-2 py-2 text-sm font-semibold',
                    rate === props.speed ? 'bg-accent text-accent-fg' : 'hover:bg-white/10',
                  )}
                >
                  {speedLabel(rate)}
                </button>
              ))}
            </div>
          </Group>
          {chapters.length > 0 && (
            <Group label="Kapitel">
              {chapters.map((chapter) => (
                <button
                  key={chapter.index}
                  type="button"
                  role="menuitem"
                  data-testid="chapter-item"
                  aria-current={chapter.index === running?.index ? 'true' : undefined}
                  onClick={pick(() => props.onChapter(chapter))}
                  className={clsx(
                    'flex w-full items-center gap-3 rounded-lg p-2 text-left text-sm hover:bg-white/10',
                    chapter.index === running?.index && 'bg-white/5',
                  )}
                >
                  <span className="aspect-video w-24 shrink-0 overflow-hidden rounded-md bg-white/10">
                    {chapter.image_url && (
                      <img src={chapter.image_url} alt="" loading="lazy" className="size-full object-cover" />
                    )}
                  </span>
                  <span className="min-w-0">
                    <span className="block truncate font-semibold">{chapterTitle(chapter)}</span>
                    <span className="block text-xs text-white/60 tabular-nums">{formatTimecode(chapter.start_ms)}</span>
                  </span>
                </button>
              ))}
            </Group>
          )}
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
