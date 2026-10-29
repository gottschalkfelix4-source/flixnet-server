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
 * the HLS transcode (natively in Safari, via hls.js elsewhere). Stops the server-side
 * transcode when the source changes or the player closes.
 */
export function useVideoSource(video: RefObject<HTMLVideoElement | null>, source: Source | undefined) {
  // Errors belong to the source they happened with, so a new source starts clean.
  const [failure, setFailure] = useState<{ url: string; message: string } | null>(null)

  useEffect(() => {
    const element = video.current
    if (!element || !source) return

    if (source.plan.method === 'direct_play' || !source.hls_url) {
      element.setAttribute('src', source.direct_url)
      return () => {
        element.removeAttribute('src')
        element.load()
      }
    }

    const url = source.hls_url
    let destroyed = false
    let destroy = () => {}
    if (element.canPlayType('application/vnd.apple.mpegurl')) {
      element.setAttribute('src', url)
      destroy = () => {
        element.removeAttribute('src')
        element.load()
      }
    } else {
      void import('hls.js').then(({ default: Hls }) => {
        if (destroyed) return
        if (!Hls.isSupported()) {
          setFailure({ url, message: 'Dein Browser unterstützt kein HLS-Streaming.' })
          return
        }
        const hls = new Hls({ maxBufferLength: 30, backBufferLength: 60 })
        hls.on(Hls.Events.ERROR, (_, data) => {
          if (data.fatal) setFailure({ url, message: 'Die Wiedergabe wurde unterbrochen.' })
        })
        hls.loadSource(url)
        hls.attachMedia(element)
        destroy = () => hls.destroy()
      })
    }
    return () => {
      destroyed = true
      destroy()
      stopSession(url)
    }
  }, [video, source])

  return failure && failure.url === source?.hls_url ? failure.message : null
}
