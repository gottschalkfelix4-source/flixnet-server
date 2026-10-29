import type { AudioTrack, SubtitleTrack } from '@/api/queries/playback'
import { languageName } from '@/lib/media'

const AUDIO_CODECS: Record<string, string> = {
  aac: 'AAC',
  ac3: 'Dolby Digital',
  eac3: 'Dolby Digital Plus',
  truehd: 'Dolby TrueHD',
  dts: 'DTS',
  flac: 'FLAC',
  opus: 'Opus',
  mp3: 'MP3',
  vorbis: 'Vorbis',
}

const SUBTITLE_FORMATS: Record<string, string> = {
  subrip: 'SRT',
  srt: 'SRT',
  ass: 'ASS',
  ssa: 'SSA',
  mov_text: 'Text',
  webvtt: 'WebVTT',
  vtt: 'WebVTT',
  smi: 'SAMI',
  hdmv_pgs_subtitle: 'PGS',
  sup: 'PGS',
  dvd_subtitle: 'VobSub',
  idx: 'VobSub',
  dvb_subtitle: 'DVB',
}

/** ISO 639-2 → BCP 47 for `<track srclang>`. */
const BCP47: Record<string, string> = {
  deu: 'de',
  ger: 'de',
  eng: 'en',
  fra: 'fr',
  fre: 'fr',
  spa: 'es',
  ita: 'it',
  jpn: 'ja',
  kor: 'ko',
  nld: 'nl',
  dut: 'nl',
  pol: 'pl',
  rus: 'ru',
  tur: 'tr',
}

function channelLabel(channels: number | null | undefined): string | null {
  if (!channels) return null
  if (channels === 1) return 'Mono'
  if (channels === 2) return 'Stereo'
  if (channels === 6) return '5.1'
  if (channels === 8) return '7.1'
  return `${channels} Kanäle`
}

export function audioLabel(track: AudioTrack): string {
  return languageName(track.language)
}

/** "Dolby Digital · 5.1 · Kommentar" */
export function audioDetail(track: AudioTrack): string {
  const codec = track.codec ? (AUDIO_CODECS[track.codec] ?? track.codec.toUpperCase()) : null
  return [codec, channelLabel(track.channels), track.title].filter(Boolean).join(' · ')
}

/** "Deutsch (erzwungen)", also the label of the `<track>` element. */
export function subtitleLabel(track: SubtitleTrack): string {
  const flags = [track.is_forced && 'erzwungen', track.is_sdh && 'SDH'].filter(Boolean)
  return flags.length ? `${languageName(track.language)} (${flags.join(', ')})` : languageName(track.language)
}

/** Whether the player can show the track at all (external picture subtitles: not yet). */
export function subtitleAvailable(track: SubtitleTrack): boolean {
  return !!track.vtt_url || track.burn_in
}

/** "PGS · wird eingebrannt · Externe Datei" */
export function subtitleDetail(track: SubtitleTrack): string {
  const format = track.codec ? (SUBTITLE_FORMATS[track.codec.toLowerCase()] ?? track.codec.toUpperCase()) : null
  return [
    format,
    track.is_external && 'Externe Datei',
    track.burn_in && 'wird ins Bild eingebrannt',
    !subtitleAvailable(track) && 'nicht unterstützt',
    track.title,
  ]
    .filter(Boolean)
    .join(' · ')
}

export function srclang(language: string | null | undefined): string | undefined {
  return language ? BCP47[language] : undefined
}

const USER_LANGUAGES = ['deu', 'ger']

/**
 * Subtitles to show without being asked: forced ones (they translate foreign-language
 * parts) in the user's language, if the player can render them itself.
 */
export function preselectedSubtitle(tracks: SubtitleTrack[]): number | null {
  const forced = tracks.find((t) => t.is_forced && t.vtt_url && USER_LANGUAGES.includes(t.language ?? ''))
  return forced?.index ?? null
}
