import { useQuery } from '@tanstack/react-query'
import { browserProfile } from '@/lib/deviceProfile'
import { api, unwrap, type Schemas } from '../client'

export type PlaybackInfo = Schemas['PlaybackInfoDto']
export type PlaybackPlan = Schemas['PlaybackPlan']
export type PlaybackReason = Schemas['Reason']
export type PlaybackSource = Schemas['MediaSourceDto']
export type AudioTrack = Schemas['AudioTrackDto']
export type SubtitleTrack = Schemas['SubtitleTrackDto']

export interface PlaybackChoice {
  audio_stream_index?: number
  subtitle_stream_index?: number
  max_bitrate?: number
}

export function usePlaybackInfo(id: number, choice: PlaybackChoice = {}) {
  return useQuery({
    queryKey: ['playback', id, choice],
    queryFn: async () =>
      unwrap(
        await api.POST('/api/v1/items/{id}/playback-info', {
          params: { path: { id } },
          body: { device_profile: browserProfile(), ...choice },
        }),
      ),
    // Signed URLs stay valid for days; no need to refetch while watching. Not kept
    // once the player is gone: closing it ends the transcode session in the answer.
    staleTime: Infinity,
    gcTime: 0,
    // Switching tracks keeps the current source on screen until the new one is there.
    placeholderData: (previous, query) => (query?.queryKey[1] === id ? previous : undefined),
    retry: false,
  })
}

/** Why a file is not played as-is, for people. */
export const REASON_LABELS: Record<PlaybackReason, string> = {
  container_not_supported: 'Dateiformat (Container) wird nicht unterstützt',
  video_codec_not_supported: 'Video-Codec wird nicht unterstützt',
  video_resolution_not_supported: 'Auflösung zu hoch für dieses Gerät',
  video_bit_depth_not_supported: 'Farbtiefe (10 Bit) wird nicht unterstützt',
  video_range_not_supported: 'HDR wird hier nicht dargestellt (Umwandlung in SDR)',
  bitrate_too_high: 'Bitrate über dem Qualitätslimit',
  audio_codec_not_supported: 'Audio-Codec wird nicht unterstützt',
  audio_channels_not_supported: 'Zu viele Audiokanäle',
  audio_track_not_default: 'Andere Tonspur als die Standardspur gewählt',
  subtitle_burn_in: 'Bild-Untertitel werden ins Video eingebrannt',
}
