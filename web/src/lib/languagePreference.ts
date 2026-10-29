import { useEffect, useRef } from 'react'
import type { PlaybackSource } from '@/api/queries/playback'
import { subtitleAvailable } from '@/lib/tracks'
import type { TrackSelection } from '@/lib/useTracks'

/** Languages the viewer picked, carried from one episode to the next. */
export interface LanguagePreference {
  /** Audio language (ISO 639-2); `undefined` = the file's default track. */
  audio?: string | null
  /** `null` = subtitles off; `undefined` = the usual preselection. */
  subtitle?: { language: string | null; forced: boolean } | null
}

let carried: { itemId: number; preference: LanguagePreference } | null = null

/** What the viewer explicitly picked in the current source (nothing picked: empty). */
export function currentPreference(source: PlaybackSource | undefined, selection: TrackSelection): LanguagePreference {
  if (!source) return {}
  const preference: LanguagePreference = {}
  if (selection.choice.audio_stream_index !== undefined) {
    const audio = source.audio_tracks.find((t) => t.index === source.plan.audio?.stream_index)
    if (audio) preference.audio = audio.language ?? null
  }
  if (selection.subtitle !== undefined) {
    const track = source.subtitle_tracks.find((t) => t.index === selection.subtitle)
    preference.subtitle = track ? { language: track.language ?? null, forced: track.is_forced } : null
  }
  return preference
}

/** Hands the preference to the player of `itemId` (autoplay of the next episode). */
export function carryPreference(itemId: number, preference: LanguagePreference) {
  carried = { itemId, preference }
}

/**
 * Applies a carried preference once the first source of `itemId` is there: the audio
 * track and subtitles in the same languages, if this episode has them.
 */
export function useCarriedPreference(
  itemId: number,
  source: PlaybackSource | undefined,
  selection: TrackSelection,
  effectiveSubtitle: number | null,
) {
  const done = useRef(false)
  useEffect(() => {
    if (done.current || !source) return
    done.current = true
    if (carried?.itemId !== itemId) return
    const { preference } = carried
    carried = null

    if (preference.audio !== undefined) {
      const current = source.audio_tracks.find((t) => t.index === source.plan.audio?.stream_index)
      const wanted = source.audio_tracks.find((t) => (t.language ?? null) === preference.audio)
      if (wanted && current?.language !== wanted.language) selection.selectAudio(wanted)
    }
    if (preference.subtitle === null) {
      if (effectiveSubtitle !== null) selection.selectSubtitle(null)
    } else if (preference.subtitle) {
      const { language, forced } = preference.subtitle
      const candidates = source.subtitle_tracks.filter((t) => (t.language ?? null) === language && subtitleAvailable(t))
      const track = candidates.find((t) => t.is_forced === forced) ?? candidates[0]
      if (track) selection.selectSubtitle(track)
    }
  }, [itemId, source, selection, effectiveSubtitle])
}
