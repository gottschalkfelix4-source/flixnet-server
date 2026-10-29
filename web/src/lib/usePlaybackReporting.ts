import { useQueryClient } from '@tanstack/react-query'
import { type RefObject, useEffect, useRef } from 'react'
import type { Schemas } from '@/api/client'
import { getSession } from '@/lib/session'

type Source = Schemas['MediaSourceDto']
type Report = Schemas['PlaybackReport']

/** Progress is reported this often (also while paused, as a heartbeat). */
const PROGRESS_INTERVAL_MS = 10_000

export interface ReportingOptions {
  itemId: number
  /** The part being played. */
  source: Source | undefined
  /** Where the current part starts on the item's timeline (multi-part movies). */
  offsetMs: number
  /** Duration of the whole item (all parts), when known. */
  totalMs: number | null
}

/** Session id of a transcode from its HLS URL. */
function transcodeSession(source: Source | undefined): string | undefined {
  return source?.hls_url?.match(/\/transcode\/([0-9a-f]{32})\//)?.[1]
}

/**
 * Raw fetch instead of the API client: the stop must survive the page closing
 * (`keepalive`), which openapi-fetch cannot ask for.
 */
async function send(path: string, body: Report, keepalive = false): Promise<Response | null> {
  const token = getSession()?.token
  if (!token) return null
  try {
    return await fetch(path, {
      method: 'POST',
      headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
      keepalive,
    })
  } catch {
    return null
  }
}

/**
 * Reports playback to the server: start on the first `playing`, progress every 10 s
 * and on pause/seek, stop when the player closes (or the page goes away). The server
 * turns this into resume points, "watched" marks and the sessions dashboard.
 *
 * Nothing is reported before playback actually starts, so a player that is closed
 * before the resume position was applied cannot overwrite a resume point.
 */
export function usePlaybackReporting(video: RefObject<HTMLVideoElement | null>, options: ReportingOptions) {
  const queryClient = useQueryClient()
  const latest = useRef(options)
  useEffect(() => {
    latest.current = options
  })
  const { itemId } = options

  useEffect(() => {
    const element = video.current
    if (!element) return
    let session: Promise<string | null> | null = null
    let stopped = false
    let lastSent = 0
    // Last known position on the item's timeline. Kept from events because the
    // element is already reset when the player unmounts.
    let position = 0

    const track = () => {
      if (element.readyState >= 1) position = latest.current.offsetMs + element.currentTime * 1000
    }
    const report = (): Report => {
      const { source, totalMs } = latest.current
      const partMs = Number.isFinite(element.duration) ? element.duration * 1000 : null
      return {
        item_id: itemId,
        position_ms: Math.max(0, Math.round(position)),
        duration_ms: Math.round(totalMs ?? partMs ?? 0) || null,
        play_method: source?.plan.method ?? 'direct_play',
        transcode_session_id: transcodeSession(source) ?? null,
        is_paused: element.paused,
      }
    }
    const progress = () => {
      if (!session || stopped) return
      track()
      lastSent = Date.now()
      const body = report()
      void session.then((id) => (id ? send(`/api/v1/playback/sessions/${id}/progress`, body) : null))
    }
    const onPlaying = () => {
      track()
      if (session) {
        progress()
        return
      }
      lastSent = Date.now()
      session = send('/api/v1/playback/sessions', report()).then(async (response) =>
        response?.ok ? ((await response.json()) as Schemas['PlaybackSessionDto']).id : null,
      )
    }
    // Pause and seek: report right away (but not twice in a row).
    const onChange = () => {
      if (Date.now() - lastSent > 1000) progress()
    }
    const stop = () => {
      if (!session || stopped) return
      stopped = true
      track()
      const body = report()
      void session
        .then((id) => (id ? send(`/api/v1/playback/sessions/${id}/stop`, body, true) : null))
        .then(() => queryClient.invalidateQueries({ queryKey: ['items'] }))
    }

    element.addEventListener('timeupdate', track)
    element.addEventListener('playing', onPlaying)
    element.addEventListener('pause', onChange)
    element.addEventListener('seeked', onChange)
    window.addEventListener('pagehide', stop)
    const timer = window.setInterval(progress, PROGRESS_INTERVAL_MS)
    return () => {
      window.clearInterval(timer)
      element.removeEventListener('timeupdate', track)
      element.removeEventListener('playing', onPlaying)
      element.removeEventListener('pause', onChange)
      element.removeEventListener('seeked', onChange)
      window.removeEventListener('pagehide', stop)
      stop()
    }
  }, [video, itemId, queryClient])
}
