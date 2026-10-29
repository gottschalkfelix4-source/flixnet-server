import type { Schemas } from '@/api/client'

export type DeviceProfile = Schemas['DeviceProfile']

/** Probe strings per codec; the first one the browser accepts wins. */
const VIDEO_PROBES: { codec: string; bitDepth: number; types: string[] }[] = [
  { codec: 'h264', bitDepth: 8, types: ['video/mp4; codecs="avc1.640033"', 'video/mp4; codecs="avc1.640028"'] },
  { codec: 'hevc', bitDepth: 10, types: ['video/mp4; codecs="hvc1.2.4.L153.B0"', 'video/mp4; codecs="hev1.2.4.L153.B0"'] },
  { codec: 'hevc', bitDepth: 8, types: ['video/mp4; codecs="hvc1.1.6.L153.B0"', 'video/mp4; codecs="hev1.1.6.L153.B0"'] },
  { codec: 'vp9', bitDepth: 10, types: ['video/webm; codecs="vp09.02.51.10"'] },
  { codec: 'vp9', bitDepth: 8, types: ['video/webm; codecs="vp9"', 'video/webm; codecs="vp09.00.51.08"'] },
  { codec: 'av1', bitDepth: 10, types: ['video/mp4; codecs="av01.0.12M.10"', 'video/webm; codecs="av01.0.12M.10"'] },
  { codec: 'vp8', bitDepth: 8, types: ['video/webm; codecs="vp8"'] },
]

const AUDIO_PROBES: { codec: string; channels: number; types: string[] }[] = [
  { codec: 'aac', channels: 6, types: ['audio/mp4; codecs="mp4a.40.2"'] },
  { codec: 'mp3', channels: 2, types: ['audio/mpeg', 'audio/mp4; codecs="mp4a.69"'] },
  { codec: 'opus', channels: 6, types: ['audio/webm; codecs="opus"', 'audio/mp4; codecs="opus"'] },
  { codec: 'vorbis', channels: 6, types: ['audio/webm; codecs="vorbis"'] },
  { codec: 'flac', channels: 6, types: ['audio/flac', 'audio/mp4; codecs="flac"'] },
  { codec: 'ac3', channels: 6, types: ['audio/mp4; codecs="ac-3"'] },
  { codec: 'eac3', channels: 6, types: ['audio/mp4; codecs="ec-3"'] },
]

const CONTAINER_PROBES: [string, string][] = [
  ['mp4', 'video/mp4'],
  ['m4v', 'video/mp4'],
  ['mov', 'video/quicktime'],
  ['webm', 'video/webm'],
]

function supported(type: string, video: HTMLVideoElement): boolean {
  // `canPlayType` covers progressive playback (Direct Play); MSE support matters for HLS later.
  const answer = video.canPlayType(type)
  if (answer === 'probably' || answer === 'maybe') return true
  return typeof MediaSource !== 'undefined' && MediaSource.isTypeSupported(type)
}

let cached: DeviceProfile | null = null

/**
 * What this browser can play without help from the server. Matroska is never listed:
 * browsers play some of it, but not reliably, and a remux costs almost nothing.
 */
export function browserProfile(): DeviceProfile {
  if (cached) return cached
  const video = document.createElement('video')
  const hdrScreen = window.matchMedia?.('(dynamic-range: high)').matches ?? false

  const videoCodecs = new Map<string, Schemas['VideoCodecSupport']>()
  for (const probe of VIDEO_PROBES) {
    if (videoCodecs.has(probe.codec) || !probe.types.some((t) => supported(t, video))) continue
    videoCodecs.set(probe.codec, {
      codec: probe.codec,
      max_width: 3840,
      max_height: 2160,
      max_bit_depth: probe.bitDepth,
      // HDR passes through only to HDR screens with a 10-bit decoder; Dolby Vision
      // needs Safari's native support.
      hdr: hdrScreen && probe.bitDepth >= 10 ? ['hdr10', 'hlg'] : [],
    })
  }

  cached = {
    containers: CONTAINER_PROBES.filter(([, type]) => video.canPlayType(type) !== '').map(([c]) => c),
    video_codecs: [...videoCodecs.values()],
    audio_codecs: AUDIO_PROBES.filter((p) => p.types.some((t) => supported(t, video))).map((p) => ({
      codec: p.codec,
      max_channels: p.channels,
    })),
    multiple_audio_tracks: false,
  }
  return cached
}
