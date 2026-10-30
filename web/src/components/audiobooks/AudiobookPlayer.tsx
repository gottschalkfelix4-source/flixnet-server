import { useQueryClient } from '@tanstack/react-query'
import clsx from 'clsx'
import { Moon, Pause, Play, RotateCcw, RotateCw } from 'lucide-react'
import { useCallback, useEffect, useRef, useState } from 'react'
import { type Audiobook, saveAudiobookProgress } from '@/api/queries/audiobooks'
import { formatTimecode } from '@/lib/format'
import { getSession } from '@/lib/session'
import { readStored, writeStored } from '@/lib/storage'

const SPEED_KEY = 'flixnet.audiobook.speed'
const SPEEDS = [0.75, 1, 1.25, 1.5, 1.75, 2]
/** Sleep timer choices in minutes; `chapter` = at the end of the current chapter. */
const SLEEP_OPTIONS: { value: string; label: string }[] = [
  { value: 'off', label: 'Aus' },
  { value: '5', label: '5 Minuten' },
  { value: '15', label: '15 Minuten' },
  { value: '30', label: '30 Minuten' },
  { value: '45', label: '45 Minuten' },
  { value: '60', label: '60 Minuten' },
  { value: 'chapter', label: 'Ende des Kapitels' },
]
const SAVE_EVERY_MS = 10_000

/** Index of the part a book position (ms) falls into. */
function partAt(parts: Audiobook['parts'], positionMs: number): number {
  for (let i = parts.length - 1; i >= 0; i--) {
    const p = parts[i]
    if (p && positionMs >= p.start_ms) return i
  }
  return 0
}

/**
 * The audiobook player: one audio element that walks through the book's parts,
 * resumes where the user left off (saved every few seconds, on pause and when
 * leaving), playback speed, ±30 s, chapters and a sleep timer.
 */
export function AudiobookPlayer({ book }: { book: Audiobook }) {
  const queryClient = useQueryClient()
  const audio = useRef<HTMLAudioElement>(null)
  const resumeMs = book.finished ? 0 : book.position_ms
  const [part, setPart] = useState(() => partAt(book.parts, resumeMs))
  // Where to jump once the part's metadata is there (offset within the part, s).
  const pendingSeek = useRef<number | null>((resumeMs - (book.parts[partAt(book.parts, resumeMs)]?.start_ms ?? 0)) / 1000)
  const autoplay = useRef(false)
  const [positionMs, setPositionMs] = useState(resumeMs)
  const [playing, setPlaying] = useState(false)
  const [started, setStarted] = useState(false)
  const [speed, setSpeed] = useState(() => readStored<number>(SPEED_KEY, 1))
  const [sleep, setSleep] = useState<{ mode: string; until: number | null; chapterEnd: number | null }>({
    mode: 'off',
    until: null,
    chapterEnd: null,
  })
  const [now, setNow] = useState(() => Date.now())
  const lastSaved = useRef(0)
  const position = useRef(resumeMs)
  const current = book.parts[part]
  const total = book.duration_ms ?? 0
  const chapter = book.chapters.findLastIndex((c) => c.start_ms <= positionMs)

  const save = useCallback(
    (keepalive = false) => {
      lastSaved.current = Date.now()
      void saveAudiobookProgress(book.id, position.current, getSession()?.token, keepalive)
    },
    [book.id],
  )

  /** Jumps to a position in the whole book. */
  const seek = useCallback(
    (ms: number) => {
      const target = Math.min(Math.max(0, ms), Math.max(0, total - 500))
      const index = partAt(book.parts, target)
      const within = (target - (book.parts[index]?.start_ms ?? 0)) / 1000
      position.current = target
      setPositionMs(target)
      if (index === part && audio.current && audio.current.readyState >= 1) {
        audio.current.currentTime = within
      } else {
        pendingSeek.current = within
        autoplay.current = !!audio.current && !audio.current.paused
        setPart(index)
      }
    },
    [book.parts, part, total],
  )

  const toggle = useCallback(() => {
    const element = audio.current
    if (!element) return
    if (element.paused) void element.play().catch(() => {})
    else element.pause()
  }, [])

  useEffect(() => {
    if (audio.current) audio.current.playbackRate = speed
    writeStored(SPEED_KEY, speed)
  }, [speed, part])

  // Save when leaving the page or the book.
  useEffect(() => {
    const onHide = () => save(true)
    window.addEventListener('pagehide', onHide)
    return () => {
      window.removeEventListener('pagehide', onHide)
      if (position.current > 0) {
        save(true)
        void queryClient.invalidateQueries({ queryKey: ['audiobooks'] })
      }
    }
  }, [save, queryClient])

  // Sleep timer.
  useEffect(() => {
    if (sleep.mode === 'off') return
    const timer = window.setInterval(() => {
      const t = Date.now()
      setNow(t)
      const timeUp = sleep.until !== null && t >= sleep.until
      const chapterOver = sleep.chapterEnd !== null && position.current >= sleep.chapterEnd
      if (timeUp || chapterOver) {
        audio.current?.pause()
        setSleep({ mode: 'off', until: null, chapterEnd: null })
      }
    }, 1000)
    return () => window.clearInterval(timer)
  }, [sleep])

  const chooseSleep = (mode: string) => {
    if (mode === 'off') setSleep({ mode, until: null, chapterEnd: null })
    else if (mode === 'chapter') {
      const end = book.chapters[chapter]?.end_ms ?? null
      setSleep({ mode, until: null, chapterEnd: end })
    } else {
      const t = Date.now()
      setNow(t)
      setSleep({ mode, until: t + Number(mode) * 60_000, chapterEnd: null })
    }
  }

  if (!current) return <p className="text-sm text-muted">Dieses Hörbuch hat keine abspielbaren Dateien.</p>

  return (
    <div className="rounded-[var(--radius-card)] border border-line/70 bg-surface/70 p-5" data-testid="audiobook-player">
      <audio
        ref={audio}
        src={current.url}
        preload="metadata"
        data-testid="audiobook-audio"
        onLoadedMetadata={(e) => {
          const element = e.currentTarget
          element.playbackRate = speed
          if (pendingSeek.current !== null) {
            element.currentTime = Math.max(0, pendingSeek.current)
            pendingSeek.current = null
          }
          if (autoplay.current) {
            autoplay.current = false
            void element.play().catch(() => {})
          }
        }}
        onTimeUpdate={(e) => {
          if (pendingSeek.current !== null) return
          const ms = current.start_ms + e.currentTarget.currentTime * 1000
          position.current = ms
          setPositionMs(ms)
          if (!e.currentTarget.paused && Date.now() - lastSaved.current > SAVE_EVERY_MS) save()
        }}
        onPlay={() => {
          setPlaying(true)
          setStarted(true)
        }}
        onPause={() => {
          setPlaying(false)
          save()
        }}
        onEnded={() => {
          if (part + 1 < book.parts.length) {
            pendingSeek.current = 0
            autoplay.current = true
            setPart(part + 1)
          } else {
            position.current = total
            save()
          }
        }}
      />

      <div className="flex items-center justify-center gap-3">
        <button
          type="button"
          aria-label="30 Sekunden zurück"
          onClick={() => seek(position.current - 30_000)}
          className="flex size-12 items-center justify-center rounded-full hover:bg-surface-2"
        >
          <RotateCcw className="size-6" />
        </button>
        <button
          type="button"
          onClick={toggle}
          aria-label={playing ? 'Pause' : resumeMs > 0 && !started ? 'Fortsetzen' : 'Abspielen'}
          className="flex size-16 items-center justify-center rounded-full bg-accent text-accent-fg hover:bg-accent-hover"
          data-testid="audiobook-play"
        >
          {playing ? <Pause className="size-8 fill-current" /> : <Play className="size-8 fill-current" />}
        </button>
        <button
          type="button"
          aria-label="30 Sekunden vor"
          onClick={() => seek(position.current + 30_000)}
          className="flex size-12 items-center justify-center rounded-full hover:bg-surface-2"
        >
          <RotateCw className="size-6" />
        </button>
      </div>

      <input
        type="range"
        min={0}
        max={Math.max(1, total)}
        step={1000}
        value={Math.min(positionMs, total)}
        onChange={(e) => seek(Number(e.target.value))}
        aria-label="Position im Hörbuch"
        aria-valuetext={formatTimecode(positionMs)}
        className="mt-4 w-full accent-[var(--color-accent)]"
      />
      <div className="flex justify-between text-xs text-muted tabular-nums">
        <span data-testid="audiobook-position">{formatTimecode(positionMs)}</span>
        <span>
          {book.chapters[chapter]?.title ?? ''} · noch {formatTimecode(Math.max(0, total - positionMs))}
        </span>
      </div>

      <div className="mt-4 flex flex-wrap items-center gap-4 text-sm">
        <label className="flex items-center gap-2">
          <span className="text-muted">Tempo</span>
          <select
            value={speed}
            onChange={(e) => setSpeed(Number(e.target.value))}
            className="h-9 rounded-full border border-line bg-surface-2 px-3"
            aria-label="Wiedergabetempo"
          >
            {SPEEDS.map((s) => (
              <option key={s} value={s}>
                {s.toLocaleString('de')}×
              </option>
            ))}
          </select>
        </label>
        <label className="flex items-center gap-2">
          <Moon className="size-4 text-muted" aria-hidden="true" />
          <span className="text-muted">Schlummer</span>
          <select
            value={sleep.mode}
            onChange={(e) => chooseSleep(e.target.value)}
            className="h-9 rounded-full border border-line bg-surface-2 px-3"
            aria-label="Schlummerfunktion"
          >
            {SLEEP_OPTIONS.map((o) => (
              <option key={o.value} value={o.value}>
                {o.label}
              </option>
            ))}
          </select>
          {sleep.until !== null && (
            <span className="text-xs text-subtle tabular-nums" data-testid="sleep-remaining">
              {formatTimecode(Math.max(0, sleep.until - now))}
            </span>
          )}
        </label>
      </div>

      {book.chapters.length > 1 && (
        <ol className="mt-6 divide-y divide-line/50" aria-label="Kapitel">
          {book.chapters.map((c, i) => (
            <li key={`${c.start_ms}-${i}`}>
              <button
                type="button"
                onClick={() => seek(c.start_ms)}
                className={clsx(
                  'flex w-full items-center gap-3 py-2.5 text-left text-sm hover:text-fg',
                  i === chapter ? 'font-bold text-accent' : 'text-muted',
                )}
                aria-current={i === chapter ? 'true' : undefined}
              >
                <span className="w-6 text-right text-xs text-subtle tabular-nums">{i + 1}</span>
                <span className="min-w-0 flex-1 truncate">{c.title}</span>
                <span className="text-xs tabular-nums">{formatTimecode(c.start_ms)}</span>
              </button>
            </li>
          ))}
        </ol>
      )}
    </div>
  )
}
