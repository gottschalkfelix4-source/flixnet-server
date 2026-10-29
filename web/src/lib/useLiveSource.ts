import { type RefObject, useEffect, useState } from 'react'
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

/**
 * Plays a live session's rolling HLS window: hls.js where Media Source works (it
 * keeps close to the live edge), native HLS otherwise (Safari, older iPhones). The
 * viewer session ends when the player closes or the session changes.
 */
export function useLiveSource(video: RefObject<HTMLVideoElement | null>, session: LiveSession | undefined) {
  const [failure, setFailure] = useState<{ id: string; message: string } | null>(null)

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
          liveSyncDurationCount: 3,
          liveMaxLatencyDurationCount: 8,
          backBufferLength: 30,
          manifestLoadingMaxRetry: 4,
          levelLoadingMaxRetry: 6,
        })
        hls.on(Hls.Events.ERROR, (_, data) => {
          if (data.fatal) setFailure({ id, message: 'Der Live-Stream wurde unterbrochen.' })
        })
        hls.loadSource(url)
        hls.attachMedia(element)
        destroy = () => hls.destroy()
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

  return failure && failure.id === session?.session_id ? failure.message : null
}
