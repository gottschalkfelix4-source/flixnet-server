import { type RefObject, useEffect, useRef, useState } from 'react'
import type { LiveSession } from '@/api/queries/livetv'
import { getSession } from '@/lib/session'

/** Ends a live viewer session; `keepalive` lets it finish while the page unloads. */
function stopLiveSession(sessionId: string) {
  const token = getSession()?.token
  if (!token) return
  void fetch(`/api/v1/livetv/sessions/${sessionId}`, {
    method: 'DELETE',
    headers: { Authorization: `Bearer ${token}` },
    keepalive: true,
  }).catch(() => {})
}

/** Distance from the newest segment the player keeps when it is "live" (seconds). */
export const LIVE_EDGE_SECS = 12

export interface LiveSource {
  /** Why the stream does not play (or `null`). */
  error: string | null
  /** Position of the live edge in `element` (seconds), if known. */
  liveEdge: (element: HTMLVideoElement) => number | null
  /** Wall-clock time of the position shown (from `EXT-X-PROGRAM-DATE-TIME`), if known. */
  playingDate: (element: HTMLVideoElement) => Date | null
}

/**
 * Plays a live session's playlist — the server's whole timeshift buffer as a live
 * (DVR window) playlist: hls.js where Media Source works, native HLS otherwise
 * (Safari, older iPhones). The player starts at the live edge, may pause and rewind
 * anywhere within the buffer, and keeps refreshing the playlist while paused, which
 * also keeps the viewer session alive. The session ends when the player closes or
 * the session changes.
 */
export function useLiveSource(video: RefObject<HTMLVideoElement | null>, session: LiveSession | undefined): LiveSource {
  const [failure, setFailure] = useState<{ id: string; message: string } | null>(null)
  const hlsRef = useRef<{ liveSyncPosition: number | null; playingDate: Date | null } | null>(null)

  useEffect(() => {
    const element = video.current
    if (!element || !session) return
    const { session_id: id, playlist_url: url } = session
    let destroyed = false
    let destroy = () => {}
    const native = element.canPlayType('application/vnd.apple.mpegurl') !== ''
    const attachNative = () => {
      element.setAttribute('src', url)
      destroy = () => {
        element.removeAttribute('src')
        element.load()
      }
    }
    void import('hls.js')
      .then(({ default: Hls }) => {
        if (destroyed) return
        if (!Hls.isSupported()) {
          if (native) attachNative()
          else setFailure({ id, message: 'Dein Browser unterstützt kein HLS-Streaming.' })
          return
        }
        const hls = new Hls({
          // Start near the live edge; never jump back to it on its own (timeshift).
          liveSyncDurationCount: 3,
          backBufferLength: 60,
          manifestLoadingMaxRetry: 4,
          levelLoadingMaxRetry: 6,
          // The upstream may restart: keep trying while the server reconnects.
          fragLoadingMaxRetry: 8,
        })
        hls.on(Hls.Events.ERROR, (_, data) => {
          if (data.fatal) setFailure({ id, message: 'Der Live-Stream wurde unterbrochen.' })
        })
        hls.loadSource(url)
        hls.attachMedia(element)
        hlsRef.current = hls
        destroy = () => {
          hlsRef.current = null
          hls.destroy()
        }
      })
      .catch(() => {
        if (destroyed) return
        if (native) attachNative()
        else setFailure({ id, message: 'Der HLS-Player ließ sich nicht laden.' })
      })
    return () => {
      destroyed = true
      destroy()
      stopLiveSession(id)
    }
  }, [video, session])

  const liveEdge = (element: HTMLVideoElement) => {
    const sync = hlsRef.current?.liveSyncPosition
    const seekable = element.seekable
    const end = seekable.length > 0 ? seekable.end(seekable.length - 1) : null
    const target = sync ?? (end !== null ? Math.max(0, end - LIVE_EDGE_SECS) : null)
    return target !== null && Number.isFinite(target) ? target : null
  }

  const playingDate = (element: HTMLVideoElement) => {
    const fromHls = hlsRef.current?.playingDate
    if (fromHls) return fromHls
    const start = (element as HTMLVideoElement & { getStartDate?: () => Date }).getStartDate?.()
    return start && !Number.isNaN(start.getTime()) ? new Date(start.getTime() + element.currentTime * 1000) : null
  }

  return {
    error: failure && failure.id === session?.session_id ? failure.message : null,
    liveEdge,
    playingDate,
  }
}
