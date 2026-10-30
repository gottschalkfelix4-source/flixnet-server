/**
 * Pure helpers for casting to a Chromecast (Default Media Receiver). No imports with
 * runtime effects, so the Playwright unit tests can load this file directly.
 */

/** Device profile sent to playback-info for the Default Media Receiver. */
export interface CastProfile {
  containers: string[]
  video_codecs: { codec: string; max_width: number; max_height: number; max_bit_depth: number; hdr: string[] }[]
  audio_codecs: { codec: string; max_channels: number }[]
  multiple_audio_tracks: boolean
}

/**
 * What every Chromecast (also the older 1080p ones) plays: H.264 up to 1080p in MP4,
 * AAC/MP3 stereo, VP8 in WebM. Everything else goes through an HLS transcode, which
 * Flixnet produces as H.264/AAC in fMP4.
 */
export function castProfile(): CastProfile {
  return {
    containers: ['mp4', 'm4v', 'webm'],
    video_codecs: [
      { codec: 'h264', max_width: 1920, max_height: 1080, max_bit_depth: 8, hdr: [] },
      { codec: 'vp8', max_width: 1920, max_height: 1080, max_bit_depth: 8, hdr: [] },
    ],
    audio_codecs: [
      { codec: 'aac', max_channels: 2 },
      { codec: 'mp3', max_channels: 2 },
      { codec: 'opus', max_channels: 2 },
      { codec: 'vorbis', max_channels: 2 },
    ],
    multiple_audio_tracks: false,
  }
}

/** Hosts a Chromecast can never reach (they point at itself). */
export function isLocalOnly(origin: string): boolean {
  try {
    const host = new URL(origin).hostname
    return host === 'localhost' || host === '[::1]' || host === '::1' || host.startsWith('127.') || host.endsWith('.localhost')
  } catch {
    return true
  }
}

/** The address cast devices load from: the configured public/LAN address, else the page's. */
export function castBase(configured: string | null | undefined, pageOrigin: string): string {
  const base = (configured?.trim() || pageOrigin).replace(/\/+$/, '')
  return base
}

/** An API path (`/api/v1/…`) as an absolute URL below `base` (which may carry a prefix). */
export function absoluteUrl(base: string, path: string): string {
  if (/^https?:\/\//i.test(path)) return path
  return `${base.replace(/\/+$/, '')}/${path.replace(/^\/+/, '')}`
}

export interface CastSource {
  plan: { method: string }
  direct_url: string
  hls_url?: string | null
  container?: string | null
}

export interface CastMedia {
  url: string
  contentType: string
  /** HLS with fMP4 segments: the receiver needs to be told. */
  hls: boolean
  /** Where to start (seconds); HLS sessions already start there. */
  startSeconds: number
}

const CONTENT_TYPES: Record<string, string> = {
  mp4: 'video/mp4',
  m4v: 'video/mp4',
  mov: 'video/mp4',
  webm: 'video/webm',
}

/** What to load on the receiver for a source from playback-info. */
export function castMedia(source: CastSource, base: string, positionMs: number): CastMedia {
  const startSeconds = Math.max(0, positionMs / 1000)
  if (source.plan.method !== 'direct_play' && source.hls_url) {
    const start = positionMs > 0 ? `?start_ms=${Math.round(positionMs)}` : ''
    return {
      url: absoluteUrl(base, `${source.hls_url}${start}`),
      contentType: 'application/x-mpegURL',
      hls: true,
      startSeconds,
    }
  }
  const container = ((source.container ?? '').split(',')[0] ?? '').trim().toLowerCase()
  return {
    url: absoluteUrl(base, source.direct_url),
    contentType: CONTENT_TYPES[container] ?? 'video/mp4',
    hls: false,
    startSeconds,
  }
}
