import { useCallback, useEffect, useRef, useState } from 'react'
import { api, unwrap } from '@/api/client'
import { absoluteUrl, castBase, castMedia, castProfile, isLocalOnly } from '@/lib/cast'
import { getSession } from '@/lib/session'

/*
 * Chromecast sender. The Google Cast Web Sender SDK is loaded from Google's CDN at
 * runtime (that is how Cast works), lazily and only in Chromium browsers; without it
 * the cast button just does not show. The minimal typings below cover what we use.
 */

interface CastStateEvent {
  castState?: string
  sessionState?: string
}
interface CastSessionLike {
  loadMedia(request: unknown): Promise<unknown>
  getCastDevice(): { friendlyName: string }
}
interface CastContextLike {
  setOptions(options: object): void
  getCastState(): string
  requestSession(): Promise<unknown>
  getCurrentSession(): CastSessionLike | null
  endCurrentSession(stopCasting: boolean): void
  addEventListener(type: string, listener: (event: CastStateEvent) => void): void
  removeEventListener(type: string, listener: (event: CastStateEvent) => void): void
}
interface RemotePlayerLike {
  isConnected: boolean
  isPaused: boolean
  currentTime: number
  duration: number
  playerState: string | null
}
interface RemotePlayerControllerLike {
  addEventListener(type: string, listener: () => void): void
  removeEventListener(type: string, listener: () => void): void
  playOrPause(): void
  seek(): void
  stop(): void
}
interface CastFramework {
  CastContext: { getInstance(): CastContextLike }
  CastContextEventType: { CAST_STATE_CHANGED: string; SESSION_STATE_CHANGED: string }
  CastState: { NO_DEVICES_AVAILABLE: string; CONNECTED: string }
  SessionState: { SESSION_STARTED: string; SESSION_RESUMED: string; SESSION_ENDED: string }
  RemotePlayer: new () => RemotePlayerLike
  RemotePlayerController: new (player: RemotePlayerLike) => RemotePlayerControllerLike
  RemotePlayerEventType: { ANY_CHANGE: string }
}
interface ChromeCast {
  AutoJoinPolicy: { ORIGIN_SCOPED: string }
  Image: new (url: string) => object
  media: {
    DEFAULT_MEDIA_RECEIVER_APP_ID: string
    MediaInfo: new (contentId: string, contentType: string) => Record<string, unknown>
    GenericMediaMetadata: new () => Record<string, unknown>
    LoadRequest: new (media: object) => Record<string, unknown>
    StreamType: { BUFFERED: string }
    HlsSegmentFormat: { FMP4: string }
    HlsVideoSegmentFormat: { FMP4: string }
  }
}
declare global {
  interface Window {
    cast?: { framework?: CastFramework }
    chrome?: { cast?: ChromeCast }
    __onGCastApiAvailable?: (available: boolean) => void
  }
}

const SDK_URL = 'https://www.gstatic.com/cv/js/sender/v1/cast_sender.js?loadCastFramework=1'
let sdk: Promise<boolean> | null = null

/** Loads the SDK once; `false` where casting is impossible (other browsers, tests). */
export function loadCastSdk(): Promise<boolean> {
  if (window.cast?.framework && window.chrome?.cast) return Promise.resolve(true)
  if (sdk) return sdk
  const chromium = /Chrome\//.test(navigator.userAgent) && !/Edg\//.test(navigator.userAgent)
  // Automated browsers (E2E) never talk to Google; tests inject a stand-in instead.
  if (!chromium || navigator.webdriver) return Promise.resolve(false)
  sdk = new Promise<boolean>((resolve) => {
    window.__onGCastApiAvailable = (available) => resolve(available && !!window.cast?.framework)
    const script = document.createElement('script')
    script.src = SDK_URL
    script.async = true
    script.onerror = () => resolve(false)
    document.head.appendChild(script)
    window.setTimeout(() => resolve(false), 15_000)
  })
  return sdk
}

export interface CastItem {
  itemId: number
  title: string
  posterUrl?: string | null
  /** Where the local player is (ms), so the TV continues there. */
  positionMs: number
  audioStreamIndex?: number
  subtitleStreamIndex?: number
}

export interface RemoteState {
  device: string
  paused: boolean
  currentTime: number
  duration: number
}

/** Progress of a cast is reported like local playback (resume points, history). */
async function report(path: string, body: object): Promise<string | null> {
  const token = getSession()?.token
  if (!token) return null
  try {
    const response = await fetch(path, {
      method: 'POST',
      headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    })
    if (!response.ok) return null
    const text = await response.text()
    return text ? ((JSON.parse(text) as { id?: string }).id ?? null) : null
  } catch {
    return null
  }
}

export function useCast() {
  const [available, setAvailable] = useState(false)
  const [devices, setDevices] = useState(false)
  const [remote, setRemote] = useState<RemoteState | null>(null)
  const [error, setError] = useState<string | null>(null)
  const controller = useRef<RemotePlayerControllerLike | null>(null)
  const player = useRef<RemotePlayerLike | null>(null)
  const reporting = useRef<{ itemId: number; session: Promise<string | null>; method: string } | null>(null)

  useEffect(() => {
    let cancelled = false
    let cleanup = () => {}
    void loadCastSdk().then((ok) => {
      const framework = window.cast?.framework
      const chromeCast = window.chrome?.cast
      if (cancelled || !ok || !framework || !chromeCast) return
      const context = framework.CastContext.getInstance()
      context.setOptions({
        receiverApplicationId: chromeCast.media.DEFAULT_MEDIA_RECEIVER_APP_ID,
        autoJoinPolicy: chromeCast.AutoJoinPolicy.ORIGIN_SCOPED,
      })
      const onCastState = () => setDevices(context.getCastState() !== framework.CastState.NO_DEVICES_AVAILABLE)
      context.addEventListener(framework.CastContextEventType.CAST_STATE_CHANGED, onCastState)
      onCastState()
      const remotePlayer = new framework.RemotePlayer()
      const remoteController = new framework.RemotePlayerController(remotePlayer)
      player.current = remotePlayer
      controller.current = remoteController
      const onChange = () => {
        const session = context.getCurrentSession()
        if (!remotePlayer.isConnected || !session) {
          setRemote(null)
          return
        }
        setRemote({
          device: session.getCastDevice().friendlyName,
          paused: remotePlayer.isPaused,
          currentTime: remotePlayer.currentTime,
          duration: remotePlayer.duration,
        })
      }
      remoteController.addEventListener(framework.RemotePlayerEventType.ANY_CHANGE, onChange)
      setAvailable(true)
      cleanup = () => {
        context.removeEventListener(framework.CastContextEventType.CAST_STATE_CHANGED, onCastState)
        remoteController.removeEventListener(framework.RemotePlayerEventType.ANY_CHANGE, onChange)
      }
    })
    return () => {
      cancelled = true
      cleanup()
    }
  }, [])

  // While casting: progress every 10 s, stop when the cast ends.
  const remotePosition = useRef(0)
  useEffect(() => {
    if (remote) remotePosition.current = remote.currentTime * 1000
  }, [remote])
  useEffect(() => {
    if (!remote) {
      const current = reporting.current
      if (current) {
        reporting.current = null
        const body = { item_id: current.itemId, position_ms: Math.round(remotePosition.current), play_method: current.method }
        void current.session.then((id) => id && report(`/api/v1/playback/sessions/${id}/stop`, body))
      }
      return
    }
    const timer = window.setInterval(() => {
      const current = reporting.current
      if (!current) return
      const body = {
        item_id: current.itemId,
        position_ms: Math.round(remotePosition.current),
        play_method: current.method,
        is_paused: player.current?.isPaused ?? false,
      }
      void current.session.then((id) => id && report(`/api/v1/playback/sessions/${id}/progress`, body))
    }, 10_000)
    return () => window.clearInterval(timer)
  }, [remote])

  /** Connects to a device (the browser's picker) and loads the item there. */
  const castItem = useCallback(async (item: CastItem) => {
    const framework = window.cast?.framework
    const chromeCast = window.chrome?.cast
    if (!framework || !chromeCast) return
    setError(null)
    const config = unwrap(await api.GET('/api/v1/cast/config'))
    const base = castBase(config.base_url, window.location.origin)
    if (isLocalOnly(base)) {
      setError(
        'Der Chromecast erreicht diese Adresse nicht. Trage unter Einstellungen → Netzwerk die LAN-Adresse des Servers ein.',
      )
      return
    }
    const context = framework.CastContext.getInstance()
    if (!context.getCurrentSession()) {
      try {
        await context.requestSession()
      } catch {
        return // picker closed
      }
    }
    const session = context.getCurrentSession()
    if (!session) return
    try {
      const info = unwrap(
        await api.POST('/api/v1/items/{id}/playback-info', {
          params: { path: { id: item.itemId } },
          body: {
            device_profile: castProfile(),
            audio_stream_index: item.audioStreamIndex,
            subtitle_stream_index: item.subtitleStreamIndex,
          },
        }),
      )
      const source = info.sources[0]
      if (!source) throw new Error('no source')
      const media = castMedia(source, base, item.positionMs)
      const mediaInfo = new chromeCast.media.MediaInfo(media.url, media.contentType)
      mediaInfo.streamType = chromeCast.media.StreamType.BUFFERED
      if (media.hls) {
        mediaInfo.hlsSegmentFormat = chromeCast.media.HlsSegmentFormat.FMP4
        mediaInfo.hlsVideoSegmentFormat = chromeCast.media.HlsVideoSegmentFormat.FMP4
      }
      const metadata = new chromeCast.media.GenericMediaMetadata()
      metadata.title = item.title
      if (item.posterUrl) metadata.images = [new chromeCast.Image(absoluteUrl(base, item.posterUrl))]
      mediaInfo.metadata = metadata
      const request = new chromeCast.media.LoadRequest(mediaInfo)
      request.currentTime = media.startSeconds
      request.autoplay = true
      await session.loadMedia(request)
      const method = source.plan.method
      reporting.current = {
        itemId: item.itemId,
        method,
        session: report('/api/v1/playback/sessions', {
          item_id: item.itemId,
          position_ms: Math.round(item.positionMs),
          play_method: method,
        }),
      }
    } catch {
      setError('Der Titel ließ sich nicht auf dem Gerät abspielen.')
    }
  }, [])

  const playOrPause = useCallback(() => controller.current?.playOrPause(), [])
  const seek = useCallback((seconds: number) => {
    if (!player.current || !controller.current) return
    player.current.currentTime = Math.max(0, seconds)
    controller.current.seek()
  }, [])
  /** Ends casting; returns where the TV was (ms) so the local player continues there. */
  const stop = useCallback(() => {
    const position = (player.current?.currentTime ?? 0) * 1000
    window.cast?.framework?.CastContext.getInstance().endCurrentSession(true)
    return position
  }, [])

  return { available: available && devices, remote, error, castItem, playOrPause, seek, stop, clearError: () => setError(null) }
}
