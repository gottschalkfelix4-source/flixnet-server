import { type RefObject, useCallback, useEffect, useRef, useState } from 'react'
import type { AudioTrack, PlaybackChoice, PlaybackSource, SubtitleTrack } from '@/api/queries/playback'
import { preselectedSubtitle, srclang, subtitleLabel } from '@/lib/tracks'

/** Where playback was when a track switch asked for a new source. */
interface ResumePoint {
  time: number
  playing: boolean
}

export interface TrackSelection {
  /** Sent with playback info: audio track and burned-in subtitles. */
  choice: PlaybackChoice
  /** Subtitle track picked in the menu: `null` = off, `undefined` = not picked yet. */
  subtitle: number | null | undefined
  selectAudio: (track: AudioTrack) => void
  selectSubtitle: (track: SubtitleTrack | null) => void
  /** Quality limit (bits/s, `undefined` = original): a new source from playback info. */
  selectQuality: (maxBitrate: number | undefined) => void
  resume: RefObject<ResumePoint | null>
}

/**
 * The viewer's track choice. Text subtitles are switched in the player alone; another
 * audio track or burned-in subtitles need a new source from playback info, which then
 * continues where the old one was.
 */
export function useTrackSelection(video: RefObject<HTMLVideoElement | null>): TrackSelection {
  const [choice, setChoice] = useState<PlaybackChoice>({})
  const [subtitle, setSubtitle] = useState<number | null | undefined>(undefined)
  const resume = useRef<ResumePoint | null>(null)

  const reload = useCallback(
    (next: PlaybackChoice) => {
      const element = video.current
      if (element && element.readyState > 0) resume.current = { time: element.currentTime, playing: !element.paused }
      setChoice(next)
    },
    [video],
  )

  const selectAudio = useCallback(
    (track: AudioTrack) => reload({ ...choice, audio_stream_index: track.index }),
    [choice, reload],
  )

  const selectSubtitle = useCallback(
    (track: SubtitleTrack | null) => {
      setSubtitle(track?.index ?? null)
      const { subtitle_stream_index: burning, ...rest } = choice
      if (track?.burn_in) reload({ ...rest, subtitle_stream_index: track.index })
      else if (burning !== undefined) reload(rest)
    },
    [choice, reload],
  )

  const selectQuality = useCallback(
    (maxBitrate: number | undefined) => {
      const next = { ...choice }
      delete next.max_bitrate
      if (maxBitrate !== undefined) next.max_bitrate = maxBitrate
      reload(next)
    },
    [choice, reload],
  )

  return { choice, subtitle, selectAudio, selectSubtitle, selectQuality, resume }
}

/**
 * Attaches the source's text subtitles as `<track>` elements (they work with direct
 * play and hls.js alike), shows the selected one and resumes playback after a switch.
 * Returns the effective subtitle track (the preselection until the viewer picks one).
 */
export function useSourceTracks(
  video: RefObject<HTMLVideoElement | null>,
  source: PlaybackSource | undefined,
  selection: TrackSelection,
): number | null {
  const subtitle =
    selection.subtitle === undefined ? preselectedSubtitle(source?.subtitle_tracks ?? []) : selection.subtitle
  const { resume } = selection

  useEffect(() => {
    const element = video.current
    if (!element || !source) return
    const nodes = source.subtitle_tracks.flatMap((track) => {
      if (!track.vtt_url) return []
      const node = document.createElement('track')
      node.kind = 'subtitles'
      node.id = `subtitle-${track.index}`
      node.label = subtitleLabel(track)
      const lang = srclang(track.language)
      if (lang) node.srclang = lang
      node.src = track.vtt_url
      element.appendChild(node)
      return [node]
    })
    return () => nodes.forEach((node) => node.remove())
  }, [video, source])

  useEffect(() => {
    const element = video.current
    if (!element) return
    for (const track of Array.from(element.textTracks)) {
      if (!track.id.startsWith('subtitle-')) continue
      const mode = track.id === `subtitle-${subtitle}` ? 'showing' : 'disabled'
      // Chrome's automatic track selection (e.g. with system caption settings) may
      // still switch on a new track until script has *changed* its mode; assigning
      // the current value is no change, so go through another one.
      if (track.mode === mode) track.mode = 'hidden'
      track.mode = mode
    }
  }, [video, source, subtitle])

  useEffect(() => {
    const element = video.current
    const point = resume.current
    if (!element || !source || !point) return
    const apply = () => {
      resume.current = null
      element.currentTime = point.time
      if (point.playing) void element.play().catch(() => {})
    }
    element.addEventListener('loadedmetadata', apply, { once: true })
    return () => element.removeEventListener('loadedmetadata', apply)
  }, [video, source, resume])

  return subtitle
}
