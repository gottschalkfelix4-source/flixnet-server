import { type RefObject, useEffect, useState } from 'react'
import type { Schemas } from '@/api/client'
import { getSession } from '@/lib/session'

type Source = Schemas['MediaSourceDto']

/** Ends a transcode session; `keepalive` lets it finish while the page unloads. */
function stopSession(hlsUrl: string) {
  const session = hlsUrl.match(/\/transcode\/([0-9a-f]{32})\//)?.[1]
  const token = getSession()?.token
  if (!session || !token) return
  void fetch(`/api/v1/transcode/${session}`, {
    method: 'DELETE',
    headers: { Authorization: `Bearer ${token}` },
    keepalive: true,
  }).catch(() => {})
}

/**
 * Attaches a source to the video element: the original file for Direct Play, otherwise
 * the HLS transcode (via hls.js where Media Source works, else natively, e.g. on older
 * iPhones). Stops the server-side
 * transcode when the source changes or the player closes.
 *
 * `startSeconds` (resume) is applied before playback begins; HLS sessions are told
 * about it so the server starts transcoding there instead of at 0.
 */
export function useVideoSource(video: RefObject<HTMLVideoElement | null>, source: Source | undefined, startSeconds = 0) {
  // Errors belong to the source they happened with, so a new source starts clean.
  const [failure, setFailure] = useState<{ url: string; message: string } | null>(null)

  useEffect(() => {
    const element = video.current
    if (!element || !source) return
    const start = startSeconds > 0 ? startSeconds : 0
    const seekToStart = () => {
      if (start > 0) element.currentTime = start
    }

    if (source.plan.method === 'direct_play' || !source.hls_url) {
      element.addEventListener('loadedmetadata', seekToStart, { once: true })
      element.setAttribute('src', source.direct_url)
      return () => {
        element.removeEventListener('loadedmetadata', seekToStart)
        element.removeAttribute('src')
        element.load()
      }
    }

    const url = source.hls_url
    const playlist = start > 0 ? `${url}?start_ms=${Math.round(start * 1000)}` : url
    let destroyed = false
    let destroy = () => {}
    const native = element.canPlayType('application/vnd.apple.mpegurl') !== ''
    const attachNative = () => {
      element.addEventListener('loadedmetadata', seekToStart, { once: true })
      element.setAttribute('src', playlist)
      destroy = () => {
        element.removeEventListener('loadedmetadata', seekToStart)
        element.removeAttribute('src')
        element.load()
      }
    }
    // hls.js first wherever Media Source works: native HLS (now also in Chrome) fetches
    // the first segments before it can seek, which would transcode from 0 on resume.
    void import('hls.js')
      .then(({ default: Hls }) => {
        if (destroyed) return
        if (!Hls.isSupported()) {
          if (native) attachNative()
          else setFailure({ url, message: 'Dein Browser unterstützt kein HLS-Streaming.' })
          return
        }
        const hls = new Hls({ maxBufferLength: 30, backBufferLength: 60, startPosition: start > 0 ? start : -1 })
        hls.on(Hls.Events.ERROR, (_, data) => {
          if (data.fatal) setFailure({ url, message: 'Die Wiedergabe wurde unterbrochen.' })
        })
        hls.loadSource(playlist)
        hls.attachMedia(element)
        destroy = () => hls.destroy()
      })
      .catch(() => {
        if (destroyed) return
        if (native) attachNative()
        else setFailure({ url, message: 'Der HLS-Player ließ sich nicht laden.' })
      })
    return () => {
      destroyed = true
      destroy()
      stopSession(url)
    }
  }, [video, source, startSeconds])

  return failure && failure.url === source?.hls_url ? failure.message : null
}
