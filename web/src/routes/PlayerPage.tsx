import { Link, useParams, useRouter, useSearch } from '@tanstack/react-router'
import clsx from 'clsx'
import { ArrowLeft, LoaderCircle, TriangleAlert } from 'lucide-react'
import { type MouseEvent, type PointerEvent, useCallback, useEffect, useRef, useState } from 'react'
import { useItem } from '@/api/queries/items'
import { REASON_LABELS, type SubtitleTrack, usePlaybackInfo } from '@/api/queries/playback'
import { useChapters, useNextEpisode, useTrickplay } from '@/api/queries/player'
import { ResumeToast } from '@/components/media/ResumeToast'
import { PlayerControls } from '@/components/player/PlayerControls'
import { PlayerOverlaySlot } from '@/components/player/PlayerOverlaySlot'
import { SettingsSheet } from '@/components/player/SettingsSheet'
import { SkipButton } from '@/components/player/SkipButton'
import { UpNextCard } from '@/components/player/UpNextCard'
import { carryPreference, currentPreference, useCarriedPreference } from '@/lib/languagePreference'
import { episodeLabel } from '@/lib/media'
import { readStored, writeStored } from '@/lib/storage'
import { subtitleAvailable } from '@/lib/tracks'
import { useMediaState } from '@/lib/useMediaState'
import { usePlaybackReporting } from '@/lib/usePlaybackReporting'
import { locate, partTimeline, useStartPosition } from '@/lib/useResumePosition'
import { useSourceTracks, useTrackSelection } from '@/lib/useTracks'
import { useUpNext } from '@/lib/useUpNext'
import { useVideoSource } from '@/lib/useVideoSource'

/** Controls hide after this long without pointer or keyboard activity. */
const HIDE_AFTER_MS = 3000
const VOLUME_KEY = 'flixnet.player.volume'

interface StoredVolume {
  volume: number
  muted: boolean
}

/** Every item gets a fresh player (state, sources, reporting), also on autoplay. */
export function PlayerPage() {
  const { id } = useParams({ from: '/play/$id' })
  return <Player key={id} id={id} />
}

/**
 * Full-screen player: the original file when the decision engine says Direct Play,
 * otherwise an HLS transcode. Own controls with trickplay previews, chapters, a
 * settings sheet (tracks, quality, speed, chapters), keyboard shortcuts and autoplay
 * of the next episode.
 */
function Player({ id }: { id: string }) {
  const { start: explicitStart } = useSearch({ from: '/play/$id' })
  const itemId = Number(id)
  const item = useItem(itemId)
  const router = useRouter()
  const container = useRef<HTMLDivElement>(null)
  const video = useRef<HTMLVideoElement>(null)
  const tracks = useTrackSelection(video)
  const playback = usePlaybackInfo(itemId, tracks.choice)
  const [chosenPart, setPart] = useState<number | null>(null)
  const [failed, setFailed] = useState(false)
  const media = useMediaState(video)

  const sources = playback.data?.sources ?? []
  // Resume: the saved position unless the link asks for one (`?start=0`: from the start).
  const timeline = partTimeline(sources)
  const startMs = useStartPosition(explicitStart, item.data?.position_ms ?? (item.isError ? 0 : undefined))
  const startAt = startMs !== null && sources.length > 0 ? locate(timeline, startMs) : null
  const part = chosenPart ?? startAt?.part ?? 0
  const source = startMs === null ? undefined : sources[part]
  const streamError = useVideoSource(video, source, chosenPart === null && startAt ? startAt.withinMs / 1000 : 0)
  usePlaybackReporting(video, { itemId, source, offsetMs: timeline.offsets[part] ?? 0, totalMs: timeline.totalMs })
  const resumedAt = explicitStart === undefined && startMs ? startMs : 0
  const restart = () => {
    if (part === 0 && video.current) video.current.currentTime = 0
    else setPart(0)
  }
  const subtitle = useSourceTracks(video, source, tracks)
  useCarriedPreference(itemId, source, tracks, subtitle)
  const error = playback.isError
    ? 'Für diesen Eintrag gibt es keine abspielbare Datei.'
    : (streamError ?? (failed ? 'Die Datei ließ sich nicht abspielen.' : null))

  const trickplay = useTrickplay(source?.file_id)
  const chapters = useChapters(source?.file_id)
  const isEpisode = item.data?.kind === 'episode'
  const next = useNextEpisode(itemId, isEpisode).data ?? null
  const lastPart = part + 1 >= sources.length

  const title = item.data
    ? [item.data.series_title, episodeLabel(item.data), item.data.title].filter(Boolean).join(' · ')
    : ''
  const back = () => (router.history.length > 1 ? router.history.back() : void router.navigate({ to: '/item/$id', params: { id } }))

  // ---- Playback actions ---------------------------------------------------------
  const togglePlay = useCallback(() => {
    const element = video.current
    if (!element) return
    if (element.paused || element.ended) void element.play().catch(() => {})
    else element.pause()
  }, [])
  const seekTo = useCallback((seconds: number) => {
    const element = video.current
    if (!element) return
    const end = Number.isFinite(element.duration) ? element.duration : seconds
    element.currentTime = Math.min(Math.max(0, seconds), Math.max(0, end - 0.1))
  }, [])
  const skip = useCallback((delta: number) => seekTo((video.current?.currentTime ?? 0) + delta), [seekTo])
  const setVolume = useCallback((volume: number) => {
    const element = video.current
    if (!element) return
    element.volume = Math.min(1, Math.max(0, volume))
    element.muted = element.volume === 0
  }, [])
  const toggleMute = useCallback(() => {
    const element = video.current
    if (!element) return
    element.muted = !element.muted
    if (!element.muted && element.volume === 0) element.volume = 0.5
  }, [])

  // Volume and mute are remembered (also for the next episode's autoplay).
  useEffect(() => {
    const element = video.current
    if (!element) return
    const stored = readStored<StoredVolume>(VOLUME_KEY, { volume: 1, muted: false })
    element.volume = Math.min(1, Math.max(0, stored.volume))
    element.muted = stored.muted
    const save = () => writeStored(VOLUME_KEY, { volume: element.volume, muted: element.muted })
    element.addEventListener('volumechange', save)
    return () => element.removeEventListener('volumechange', save)
  }, [])

  // Speed survives source switches (a new source resets to the default rate).
  const [speed, setSpeed] = useState(1)
  useEffect(() => {
    const element = video.current
    if (!element) return
    element.defaultPlaybackRate = speed
    element.playbackRate = speed
  }, [speed, source])

  // ---- Fullscreen and picture-in-picture ------------------------------------------
  const [fullscreen, setFullscreen] = useState(false)
  useEffect(() => {
    const update = () => setFullscreen(document.fullscreenElement === container.current)
    document.addEventListener('fullscreenchange', update)
    return () => document.removeEventListener('fullscreenchange', update)
  }, [])
  const toggleFullscreen = useCallback(() => {
    if (document.fullscreenElement) void document.exitFullscreen().catch(() => {})
    else if (container.current?.requestFullscreen) void container.current.requestFullscreen().catch(() => {})
    else {
      // iPhone Safari: only the video element itself can go full screen.
      const element = video.current as (HTMLVideoElement & { webkitEnterFullscreen?: () => void }) | null
      element?.webkitEnterFullscreen?.()
    }
  }, [])
  const pipAvailable = typeof document !== 'undefined' && document.pictureInPictureEnabled === true
  const togglePip = useCallback(() => {
    if (document.pictureInPictureElement) void document.exitPictureInPicture().catch(() => {})
    else void video.current?.requestPictureInPicture().catch(() => {})
  }, [])

  // ---- Next episode ----------------------------------------------------------------
  const playNext = useCallback(() => {
    if (!next) return
    carryPreference(next.id, currentPreference(source, tracks))
    void router.navigate({ to: '/play/$id', params: { id: String(next.id) }, replace: true })
  }, [next, source, tracks, router])
  // The detected credits start, else "30 s before the end".
  const creditsStart = source?.segments.find((s) => s.kind === 'credits')?.start_ms
  const upNext = useUpNext(
    video,
    !!next && lastPart && !error,
    playNext,
    creditsStart === undefined ? undefined : creditsStart / 1000,
  )

  // ---- Subtitles on/off (C) --------------------------------------------------------
  const lastSubtitle = useRef<number | null>(null)
  useEffect(() => {
    if (subtitle !== null) lastSubtitle.current = subtitle
  }, [subtitle])
  const toggleSubtitles = useCallback(() => {
    if (!source) return
    if (subtitle !== null) {
      tracks.selectSubtitle(null)
      return
    }
    const usable = source.subtitle_tracks.filter((t) => t.vtt_url && subtitleAvailable(t))
    const pick: SubtitleTrack | undefined =
      usable.find((t) => t.index === lastSubtitle.current) ??
      usable.find((t) => !t.is_forced && ['deu', 'ger'].includes(t.language ?? '')) ??
      usable.find((t) => !t.is_forced) ??
      usable[0]
    if (pick) tracks.selectSubtitle(pick)
  }, [source, subtitle, tracks])

  // ---- Auto-hide -----------------------------------------------------------------
  const [active, setActive] = useState(true)
  const [settingsOpen, setSettingsOpen] = useState(false)
  const hideTimer = useRef<number | undefined>(undefined)
  const poke = useCallback(() => {
    setActive(true)
    window.clearTimeout(hideTimer.current)
    hideTimer.current = window.setTimeout(() => setActive(false), HIDE_AFTER_MS)
  }, [])
  useEffect(() => {
    hideTimer.current = window.setTimeout(() => setActive(false), HIDE_AFTER_MS)
    return () => window.clearTimeout(hideTimer.current)
  }, [])
  const controlsVisible = active || media.paused || settingsOpen || !source || !!error

  // ---- Keyboard shortcuts ----------------------------------------------------------
  const keys = useRef<(event: KeyboardEvent) => void>(() => {})
  useEffect(() => {
    keys.current = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.ctrlKey || event.metaKey || event.altKey || settingsOpen) return
      const target = event.target as HTMLElement | null
      if (target?.closest('input, textarea, select, [contenteditable="true"]')) return
      // Space and Enter on a focused button press that button.
      const onButton = !!target?.closest('button, a')
      const element = video.current
      let handled = true
      switch (event.key) {
        case ' ':
          if (onButton) return
          togglePlay()
          break
        case 'k':
        case 'K':
          togglePlay()
          break
        case 'j':
        case 'J':
          skip(-10)
          break
        case 'l':
        case 'L':
          skip(10)
          break
        case 'ArrowLeft':
          skip(-5)
          break
        case 'ArrowRight':
          skip(5)
          break
        case 'ArrowUp':
          if (element) setVolume(element.volume + 0.1)
          break
        case 'ArrowDown':
          if (element) setVolume(element.volume - 0.1)
          break
        case 'f':
        case 'F':
          toggleFullscreen()
          break
        case 'm':
        case 'M':
          toggleMute()
          break
        case 'c':
        case 'C':
          toggleSubtitles()
          break
        case 'n':
        case 'N':
          if (next) playNext()
          else handled = false
          break
        default:
          handled = false
      }
      if (handled) {
        event.preventDefault()
        poke()
      }
    }
  })
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => keys.current(event)
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])

  // Mouse: click toggles playback, double click full screen. Touch: a tap shows or
  // hides the controls.
  const pointerType = useRef('mouse')
  const onSurfacePointerDown = (event: PointerEvent) => {
    pointerType.current = event.pointerType
  }
  const onSurfaceClick = () => {
    if (pointerType.current === 'mouse') {
      togglePlay()
      poke()
    } else if (controlsVisible && !media.paused) {
      window.clearTimeout(hideTimer.current)
      setActive(false)
    } else {
      poke()
    }
  }

  // Hidden controls stay where they are (only transparent). A tap (touch, pen) on
  // them first just shows them instead of pressing an invisible button; a mouse shows
  // them on its first movement anyway.
  const revealTap = useRef(false)
  const onPointerDownCapture = (event: PointerEvent) => {
    revealTap.current = event.pointerType !== 'mouse' && !controlsVisible
    if (revealTap.current) {
      event.stopPropagation()
      poke()
    }
  }
  const onClickCapture = (event: MouseEvent) => {
    if (!revealTap.current) return
    revealTap.current = false
    event.stopPropagation()
    event.preventDefault()
  }

  return (
    <div
      ref={container}
      className={clsx('fixed inset-0 bg-black text-white select-none', !controlsVisible && 'cursor-none')}
      onPointerMove={(event) => event.pointerType === 'mouse' && poke()}
      onPointerDownCapture={onPointerDownCapture}
      onClickCapture={onClickCapture}
      onKeyDown={poke}
      data-testid="player"
      data-controls={controlsVisible ? 'visible' : 'hidden'}
    >
      <video
        ref={video}
        autoPlay
        playsInline
        hidden={!!error}
        className="size-full"
        data-testid="player-video"
        onEnded={() => !lastPart && setPart(part + 1)}
        onError={() => setFailed(true)}
      />
      {!error && (
        <div
          className="absolute inset-0"
          aria-hidden="true"
          onPointerDown={onSurfacePointerDown}
          onClick={onSurfaceClick}
          onDoubleClick={toggleFullscreen}
        />
      )}
      {!error && source && media.waiting && (
        <div className="pointer-events-none absolute inset-0 flex items-center justify-center" role="status" aria-label="Lädt …">
          <LoaderCircle className="size-14 animate-spin text-white/80" aria-hidden="true" />
        </div>
      )}

      <header
        className={clsx(
          'absolute inset-x-0 top-0 z-10 flex items-center gap-3 bg-gradient-to-b from-black/80 to-transparent p-4 transition-opacity duration-300',
          controlsVisible ? 'opacity-100' : 'opacity-0',
        )}
      >
        <button
          type="button"
          onClick={back}
          aria-label="Zurück"
          className="flex size-11 items-center justify-center rounded-full hover:bg-white/15"
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
        {source && (
          <span className="ml-auto shrink-0 rounded bg-white/15 px-2 py-0.5 text-xs font-semibold whitespace-nowrap" data-testid="play-method">
            {source.plan.method === 'direct_play' ? 'Direct Play' : source.plan.method === 'remux' ? 'Remux' : 'Transcode'}
          </span>
        )}
      </header>

      {resumedAt > 0 && !error && <ResumeToast positionMs={resumedAt} onRestart={restart} />}

      {!error && (
        <PlayerOverlaySlot controlsVisible={controlsVisible}>
          {source && <SkipButton videoRef={video} segments={source.segments} />}
          {next && upNext.visible && (
            <UpNextCard episode={next} secondsLeft={upNext.secondsLeft} onPlayNow={playNext} onCancel={upNext.cancel} />
          )}
        </PlayerOverlaySlot>
      )}

      {source && !error && (
        <div
          className={clsx(
            'pointer-events-none absolute inset-x-0 bottom-0 z-30 bg-gradient-to-t from-black/85 via-black/50 to-transparent pt-16 transition-opacity duration-300',
            controlsVisible ? 'opacity-100' : 'opacity-0',
          )}
        >
          <PlayerControls
            media={media}
            chapters={chapters.data ?? []}
            trickplay={trickplay.data}
            fullscreen={fullscreen}
            pipAvailable={pipAvailable}
            nextLabel={next ? [episodeLabel(next), next.title].filter(Boolean).join(' · ') : null}
            onTogglePlay={togglePlay}
            onSeek={seekTo}
            onSkip={skip}
            onToggleMute={toggleMute}
            onVolume={setVolume}
            onToggleFullscreen={toggleFullscreen}
            onTogglePip={togglePip}
            onNext={playNext}
            settings={
              <SettingsSheet
                open={settingsOpen}
                onOpenChange={setSettingsOpen}
                source={source}
                subtitle={subtitle}
                onAudio={tracks.selectAudio}
                onSubtitle={tracks.selectSubtitle}
                maxBitrate={tracks.choice.max_bitrate}
                onQuality={tracks.selectQuality}
                speed={speed}
                onSpeed={setSpeed}
                chapters={chapters.data ?? []}
                positionMs={media.currentTime * 1000}
                onChapter={(chapter) => seekTo(chapter.start_ms / 1000)}
              />
            }
          />
        </div>
      )}

      {error && (
        <div className="flex size-full flex-col items-center justify-center gap-3 p-6 text-center" role="alert">
          <TriangleAlert className="size-10 text-amber-400" aria-hidden="true" />
          <p className="text-lg font-bold">{error}</p>
          {source && source.plan.reasons.length > 0 && (
            <ul className="max-w-md space-y-1 text-sm text-white/70" data-testid="playback-reasons">
              {source.plan.reasons.map((reason) => (
                <li key={reason}>{REASON_LABELS[reason]}</li>
              ))}
            </ul>
          )}
          <Link to="/item/$id" params={{ id }} className="mt-2 rounded-full bg-white/15 px-5 py-2 text-sm font-semibold hover:bg-white/25">
            Zur Detailseite
          </Link>
        </div>
      )}
    </div>
  )
}
