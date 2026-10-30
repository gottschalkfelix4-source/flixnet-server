import { Link } from '@tanstack/react-router'
import clsx from 'clsx'
import {
  Disc3,
  ListMusic,
  LoaderCircle,
  MicVocal,
  Pause,
  Play,
  Repeat,
  Repeat1,
  Shuffle,
  SkipBack,
  SkipForward,
  Volume1,
  Volume2,
  VolumeX,
  X,
} from 'lucide-react'
import { type ReactNode, useEffect, useRef, useState } from 'react'
import { useLyrics } from '@/api/queries/music'
import { formatTimecode } from '@/lib/format'
import { type ReplayGainMode, musicPlayer } from '@/lib/music/player'
import { usePlayerState } from '@/lib/music/usePlayer'

type Panel = 'queue' | 'lyrics' | null

function IconButton({
  label,
  onClick,
  active = false,
  children,
  testId,
  large = false,
}: {
  label: string
  onClick: () => void
  active?: boolean
  children: ReactNode
  testId?: string
  large?: boolean
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      aria-pressed={active || undefined}
      data-testid={testId}
      onClick={onClick}
      className={clsx(
        'flex shrink-0 items-center justify-center rounded-full transition-colors',
        large ? 'size-10 bg-fg text-canvas hover:bg-white' : 'size-9 hover:bg-surface-3',
        !large && (active ? 'text-accent' : 'text-muted hover:text-fg'),
      )}
    >
      {children}
    </button>
  )
}

/** Persistent player bar at the bottom of every page while something is queued. */
export function MiniPlayer() {
  const state = usePlayerState()
  const [panel, setPanel] = useState<Panel>(null)
  const track = state.queue[state.index]
  if (!track) return null

  const toggle = (p: Panel) => setPanel((current) => (current === p ? null : p))
  const VolumeIcon = state.muted || state.volume === 0 ? VolumeX : state.volume < 0.5 ? Volume1 : Volume2
  const RepeatIcon = state.repeat === 'one' ? Repeat1 : Repeat

  return (
    <>
      {panel === 'queue' && <QueuePanel onClose={() => setPanel(null)} />}
      {panel === 'lyrics' && <LyricsPanel trackId={track.id} onClose={() => setPanel(null)} />}
      <div
        className="fixed inset-x-0 bottom-0 z-40 border-t border-line bg-sidebar/95 backdrop-blur"
        data-testid="mini-player"
        role="region"
        aria-label="Musikplayer"
      >
        <SeekBar />
        <div className="flex items-center gap-3 px-3 py-2 md:px-5">
          <div className="flex min-w-0 flex-1 items-center gap-3">
            <div className="relative size-12 shrink-0 overflow-hidden rounded-md bg-surface-2">
              {track.cover_url ? (
                <img src={track.cover_url} alt="" className="size-full object-cover" />
              ) : (
                <Disc3 className="absolute inset-0 m-auto size-6 text-subtle" aria-hidden="true" />
              )}
            </div>
            <div className="min-w-0">
              {track.album_id != null ? (
                <Link
                  to="/music/album/$id"
                  params={{ id: String(track.album_id) }}
                  className="block truncate text-sm font-semibold hover:underline"
                  data-testid="player-track-title"
                >
                  {track.title}
                </Link>
              ) : (
                <p className="truncate text-sm font-semibold" data-testid="player-track-title">
                  {track.title}
                </p>
              )}
              {track.album_artist_id != null ? (
                <Link
                  to="/music/artist/$id"
                  params={{ id: String(track.album_artist_id) }}
                  className="block truncate text-xs text-muted hover:underline"
                >
                  {track.artist}
                </Link>
              ) : (
                <p className="truncate text-xs text-muted">{track.artist}</p>
              )}
              {state.error && (
                <p role="alert" className="truncate text-xs text-red-400">
                  {state.error}
                </p>
              )}
            </div>
          </div>

          <div className="flex items-center gap-1">
            <IconButton label="Zufallswiedergabe" active={state.shuffle} onClick={() => musicPlayer.toggleShuffle()}>
              <Shuffle className="size-4" />
            </IconButton>
            <IconButton label="Vorheriger Titel" onClick={() => musicPlayer.previous()} testId="player-previous">
              <SkipBack className="size-5" fill="currentColor" />
            </IconButton>
            <IconButton
              label={state.playing ? 'Pause' : 'Abspielen'}
              onClick={() => musicPlayer.toggle()}
              testId="player-play"
              large
            >
              {state.loading && state.playing ? (
                <LoaderCircle className="size-5 animate-spin" />
              ) : state.playing ? (
                <Pause className="size-5" fill="currentColor" />
              ) : (
                <Play className="size-5 translate-x-px" fill="currentColor" />
              )}
            </IconButton>
            <IconButton label="Nächster Titel" onClick={() => musicPlayer.next()} testId="player-next">
              <SkipForward className="size-5" fill="currentColor" />
            </IconButton>
            <IconButton
              label={state.repeat === 'one' ? 'Titel wiederholen' : state.repeat === 'all' ? 'Alle wiederholen' : 'Wiederholen aus'}
              active={state.repeat !== 'off'}
              onClick={() => musicPlayer.cycleRepeat()}
            >
              <RepeatIcon className="size-4" />
            </IconButton>
          </div>

          <div className="hidden flex-1 items-center justify-end gap-1 md:flex">
            <span className="mr-2 text-xs text-subtle tabular-nums" data-testid="player-time">
              {formatTimecode(state.currentTime * 1000)} / {formatTimecode(state.duration * 1000)}
            </span>
            <IconButton
              label="Liedtext"
              active={panel === 'lyrics'}
              onClick={() => toggle('lyrics')}
              testId="player-lyrics"
            >
              <MicVocal className={clsx('size-4', !track.has_lyrics && 'opacity-40')} />
            </IconButton>
            <IconButton label="Warteschlange" active={panel === 'queue'} onClick={() => toggle('queue')} testId="player-queue">
              <ListMusic className="size-4" />
            </IconButton>
            <IconButton label={state.muted ? 'Ton an' : 'Stumm'} onClick={() => musicPlayer.toggleMute()}>
              <VolumeIcon className="size-4" />
            </IconButton>
            <input
              type="range"
              min={0}
              max={1}
              step={0.01}
              aria-label="Lautstärke"
              value={state.muted ? 0 : state.volume}
              onChange={(e) => musicPlayer.setVolume(Number(e.target.value))}
              className="w-24 accent-[var(--color-accent)]"
            />
          </div>
        </div>
      </div>
    </>
  )
}

function SeekBar() {
  const state = usePlayerState()
  const [dragging, setDragging] = useState<number | null>(null)
  const duration = state.duration || 0
  const value = dragging ?? state.currentTime
  return (
    <input
      type="range"
      aria-label="Position"
      data-testid="player-seek"
      min={0}
      max={duration || 1}
      step={0.1}
      value={Math.min(value, duration || 1)}
      onChange={(e) => setDragging(Number(e.target.value))}
      onPointerUp={() => {
        if (dragging !== null) musicPlayer.seek(dragging)
        setDragging(null)
      }}
      onKeyUp={() => {
        if (dragging !== null) musicPlayer.seek(dragging)
        setDragging(null)
      }}
      className="absolute inset-x-0 -top-1.5 h-3 w-full cursor-pointer appearance-none bg-transparent accent-[var(--color-accent)] [&::-webkit-slider-runnable-track]:h-1 [&::-webkit-slider-runnable-track]:bg-surface-3 [&::-webkit-slider-thumb]:-mt-1 [&::-webkit-slider-thumb]:size-3 [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:bg-accent"
      style={{
        background: `linear-gradient(to right, var(--color-accent) ${duration ? (value / duration) * 100 : 0}%, transparent 0)`,
        backgroundSize: '100% 4px',
        backgroundPosition: 'center',
        backgroundRepeat: 'no-repeat',
      }}
    />
  )
}

function PanelFrame({ title, onClose, children, testId }: { title: string; onClose: () => void; children: ReactNode; testId: string }) {
  return (
    <aside
      className="fixed right-0 bottom-[68px] z-40 flex max-h-[70vh] w-full flex-col border-l border-t border-line bg-surface shadow-2xl md:right-4 md:w-96 md:rounded-t-xl md:border-r"
      aria-label={title}
      data-testid={testId}
    >
      <div className="flex items-center justify-between border-b border-line px-4 py-3">
        <h2 className="text-sm font-bold">{title}</h2>
        <button type="button" aria-label="Schließen" onClick={onClose} className="text-muted hover:text-fg">
          <X className="size-4" />
        </button>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto">{children}</div>
    </aside>
  )
}

function QueuePanel({ onClose }: { onClose: () => void }) {
  const state = usePlayerState()
  const modes: { value: ReplayGainMode; label: string }[] = [
    { value: 'off', label: 'Aus' },
    { value: 'track', label: 'Titel' },
    { value: 'album', label: 'Album' },
  ]
  return (
    <PanelFrame title="Warteschlange" onClose={onClose} testId="queue-panel">
      <ol className="py-1">
        {state.queue.map((t, i) => (
          <li
            key={`${t.id}-${i}`}
            className={clsx('group flex items-center gap-3 px-4 py-1.5 hover:bg-surface-2', i === state.index && 'text-accent')}
            data-testid="queue-entry"
          >
            <button type="button" className="min-w-0 flex-1 text-left" onClick={() => musicPlayer.jumpTo(i)}>
              <p className="truncate text-sm font-semibold">{t.title}</p>
              <p className="truncate text-xs text-subtle">{t.artist}</p>
            </button>
            {i !== state.index && (
              <button
                type="button"
                aria-label={`${t.title} entfernen`}
                onClick={() => musicPlayer.removeAt(i)}
                className="hidden text-subtle hover:text-fg group-hover:block"
              >
                <X className="size-4" />
              </button>
            )}
          </li>
        ))}
      </ol>
      <div className="flex items-center justify-between gap-3 border-t border-line px-4 py-3 text-xs">
        <label className="flex items-center gap-2 text-muted">
          ReplayGain
          <select
            value={state.replayGain}
            onChange={(e) => musicPlayer.setReplayGain(e.target.value as ReplayGainMode)}
            className="rounded border border-line bg-surface-2 px-2 py-1 text-fg"
          >
            {modes.map((m) => (
              <option key={m.value} value={m.value}>
                {m.label}
              </option>
            ))}
          </select>
        </label>
        <button
          type="button"
          onClick={() => {
            musicPlayer.clear()
            onClose()
          }}
          className="font-semibold text-muted hover:text-fg"
        >
          Leeren
        </button>
      </div>
    </PanelFrame>
  )
}

function LyricsPanel({ trackId, onClose }: { trackId: number; onClose: () => void }) {
  const lyrics = useLyrics(trackId)
  const state = usePlayerState()
  const list = useRef<HTMLOListElement>(null)
  const timeMs = state.currentTime * 1000
  const lines = lyrics.data?.lines ?? []
  const synced = lyrics.data?.synced ?? false
  let active = -1
  if (synced) {
    for (let i = 0; i < lines.length; i++) {
      if ((lines[i]?.time_ms ?? Infinity) <= timeMs + 150) active = i
      else break
    }
  }

  useEffect(() => {
    if (active < 0) return
    list.current
      ?.querySelector<HTMLElement>(`[data-line="${active}"]`)
      ?.scrollIntoView({ block: 'center', behavior: 'smooth' })
  }, [active])

  return (
    <PanelFrame title="Liedtext" onClose={onClose} testId="lyrics-panel">
      {lyrics.isLoading && <p className="p-4 text-sm text-muted">Lädt…</p>}
      {lyrics.isError && <p className="p-4 text-sm text-muted">Für diesen Titel gibt es keinen Liedtext.</p>}
      {lyrics.data && (
        <ol ref={list} className="space-y-2 px-4 py-4" data-synced={synced || undefined}>
          {lines.map((line, i) => (
            <li
              key={i}
              data-line={i}
              data-active={i === active || undefined}
              className={clsx(
                'text-base leading-snug transition-colors',
                !synced && 'text-fg',
                synced && (i === active ? 'font-bold text-fg' : 'text-subtle'),
                synced && 'cursor-pointer hover:text-muted',
                !line.text && 'h-3',
              )}
              onClick={synced ? () => musicPlayer.seek(line.time_ms / 1000) : undefined}
            >
              {line.text}
            </li>
          ))}
        </ol>
      )}
    </PanelFrame>
  )
}
