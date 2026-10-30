import { type KeyboardEvent, type PointerEvent, useState } from 'react'
import type { Chapter, Trickplay } from '@/api/queries/player'
import { formatTimecode } from '@/lib/format'
import { chapterAt, chapterTitle, tileAt } from '@/lib/trickplay'

interface SeekBarProps {
  /** Seconds. */
  currentTime: number
  duration: number
  bufferedEnd: number
  chapters: Chapter[]
  trickplay: Trickplay | null | undefined
  onSeek: (seconds: number) => void
}

/** A position under the pointer: time, and pixel offset within the bar. */
interface Point {
  time: number
  x: number
  width: number
}

/** Width the preview picture is shown at (CSS pixels). */
const PREVIEW_WIDTH = 240

/**
 * Seek bar: played and buffered range, chapter marks, and on hover or while dragging
 * a preview with the trickplay thumbnail, time and chapter. Seeks once on release,
 * so dragging across an HLS transcode does not restart ffmpeg for every pixel.
 */
export function SeekBar({ currentTime, duration, bufferedEnd, chapters, trickplay, onSeek }: SeekBarProps) {
  const [hover, setHover] = useState<Point | null>(null)
  const [dragging, setDragging] = useState<Point | null>(null)

  const known = duration > 0
  const pointAt = (event: PointerEvent<HTMLDivElement>): Point => {
    const rect = event.currentTarget.getBoundingClientRect()
    const x = Math.min(rect.width, Math.max(0, event.clientX - rect.left))
    return { time: rect.width > 0 ? (x / rect.width) * duration : 0, x, width: rect.width }
  }

  const onPointerDown = (event: PointerEvent<HTMLDivElement>) => {
    if (!known || event.button !== 0) return
    event.currentTarget.setPointerCapture(event.pointerId)
    const point = pointAt(event)
    setDragging(point)
    setHover(point)
  }
  const onPointerMove = (event: PointerEvent<HTMLDivElement>) => {
    if (!known) return
    const point = pointAt(event)
    if (dragging) setDragging(point)
    if (event.pointerType === 'mouse' || dragging) setHover(point)
  }
  const onPointerUp = (event: PointerEvent<HTMLDivElement>) => {
    if (!dragging) return
    onSeek(pointAt(event).time)
    setDragging(null)
    if (event.pointerType !== 'mouse') setHover(null)
  }
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === 'Home') onSeek(0)
    else if (event.key === 'End') onSeek(Math.max(0, duration - 1))
    else if (event.key === 'PageUp') onSeek(Math.min(duration, currentTime + 60))
    else if (event.key === 'PageDown') onSeek(Math.max(0, currentTime - 60))
    else return
    event.preventDefault()
    event.stopPropagation()
  }

  const shown = dragging?.time ?? currentTime
  const percent = (seconds: number) => (known ? `${Math.min(100, Math.max(0, (seconds / duration) * 100))}%` : '0%')
  const preview = dragging ?? hover
  const previewMs = (preview?.time ?? 0) * 1000
  const tile = preview && trickplay ? tileAt(trickplay, previewMs) : null
  const chapter = preview ? chapterAt(chapters, previewMs) : undefined
  const scale = tile ? PREVIEW_WIDTH / tile.width : 1
  // Keep the bubble inside the bar.
  const half = (tile ? PREVIEW_WIDTH : 80) / 2
  const left = preview ? Math.min(Math.max(preview.x, half), Math.max(half, preview.width - half)) : 0

  return (
    <div
      role="slider"
      tabIndex={0}
      aria-label="Wiedergabeposition"
      aria-valuemin={0}
      aria-valuemax={Math.round(duration)}
      aria-valuenow={Math.round(shown)}
      aria-valuetext={`${formatTimecode(shown * 1000)} von ${formatTimecode(duration * 1000)}`}
      data-testid="seek-bar"
      className="group/seek relative flex h-6 w-full cursor-pointer touch-none items-center"
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={() => setDragging(null)}
      onPointerLeave={() => !dragging && setHover(null)}
      onKeyDown={onKeyDown}
    >
      <div className="relative h-1 w-full overflow-hidden rounded-full bg-white/25 transition-[height] group-hover/seek:h-1.5">
        <div className="absolute inset-y-0 left-0 bg-white/40" style={{ width: percent(bufferedEnd) }} />
        <div className="absolute inset-y-0 left-0 bg-accent" style={{ width: percent(shown) }} />
        {known &&
          chapters
            .filter((c) => c.start_ms > 0 && c.start_ms / 1000 < duration)
            .map((c) => (
              <span
                key={c.index}
                data-testid="chapter-marker"
                className="absolute inset-y-0 w-0.5 -translate-x-1/2 bg-black/70"
                style={{ left: percent(c.start_ms / 1000) }}
              />
            ))}
      </div>
      <span
        aria-hidden="true"
        className="pointer-events-none absolute top-1/2 size-3.5 -translate-x-1/2 -translate-y-1/2 rounded-full bg-accent shadow transition-transform group-hover/seek:scale-110"
        style={{ left: percent(shown) }}
      />

      {preview && known && (
        <div
          className="pointer-events-none absolute bottom-7 flex -translate-x-1/2 flex-col items-center gap-1"
          style={{ left }}
          data-testid="seek-preview"
          aria-hidden="true"
        >
          {tile && (
            <div
              data-testid="trickplay-preview"
              className="overflow-hidden rounded-lg bg-black shadow-xl ring-1 ring-white/20"
              style={{
                width: PREVIEW_WIDTH,
                height: Math.round(tile.height * scale),
                backgroundImage: `url("${tile.url}")`,
                backgroundSize: `${tile.sheetWidth * scale}px ${tile.sheetHeight * scale}px`,
                backgroundPosition: `-${tile.x * scale}px -${tile.y * scale}px`,
              }}
            />
          )}
          {chapter && (
            <span className="max-w-60 truncate text-xs font-semibold text-white/85" data-testid="preview-chapter">
              {chapterTitle(chapter)}
            </span>
          )}
          <span className="rounded bg-black/80 px-1.5 py-0.5 text-xs font-semibold tabular-nums" data-testid="preview-time">
            {formatTimecode(previewMs)}
          </span>
        </div>
      )}
    </div>
  )
}
