import { type RefObject, useCallback, useEffect, useRef, useState } from 'react'
import { getSession } from '@/lib/session'
import {
  type ClockSample,
  type GroupState,
  HARD_SEEK_MS,
  bestOffset,
  clockSample,
  expectedPosition,
  socketUrl,
  startDelay,
} from '@/lib/syncplay'

export interface Reaction {
  key: number
  emoji: string
  name: string
}

export interface SyncPlayHandle {
  connected: boolean
  state: GroupState | null
  memberId: number | null
  /** Why the group is gone or a command was refused (German). */
  error: string | null
  closed: boolean
  reactions: Reaction[]
  play: () => void
  pause: () => void
  seek: (seconds: number) => void
  setItem: (itemId: number) => void
  react: (emoji: string) => void
}

type ServerMessage =
  | { type: 'welcome'; member_id: number }
  | { type: 'state'; state: GroupState }
  | { type: 'pong'; client_ms: number; server_ms: number }
  | { type: 'correction'; rate: number; seek_to_ms: number | null }
  | { type: 'reaction'; member_id: number; name: string; emoji: string }
  | { type: 'error'; message: string }
  | { type: 'closed'; reason: string }

/**
 * Follows a SyncPlay group with the given video element: applies the group state
 * (play, pause, position, scheduled starts), reports buffering/ready and the position
 * (the server answers with rate nudges or seeks), keeps the clock in sync and refuses
 * local play/pause that did not come from the group.
 *
 * `onItem` is called when the group switched to another item (next episode).
 */
export function useSyncPlay(
  video: RefObject<HTMLVideoElement | null>,
  groupId: string | null,
  itemId: number,
  onItem: (itemId: number) => void,
): SyncPlayHandle | null {
  const [connected, setConnected] = useState(false)
  const [state, setState] = useState<GroupState | null>(null)
  const [memberId, setMemberId] = useState<number | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [closed, setClosed] = useState(false)
  const [reactions, setReactions] = useState<Reaction[]>([])
  const socket = useRef<WebSocket | null>(null)
  const latest = useRef({ itemId, onItem })
  useEffect(() => {
    latest.current = { itemId, onItem }
  })

  const send = useCallback((message: object) => {
    const ws = socket.current
    if (ws?.readyState === WebSocket.OPEN) ws.send(JSON.stringify(message))
  }, [])

  useEffect(() => {
    const element = video.current
    const token = getSession()?.token
    if (!groupId || !element || !token) return
    const ws = new WebSocket(socketUrl(window.location, groupId), ['flixnet', `flixnet.auth.${token}`])
    socket.current = ws
    const samples: ClockSample[] = []
    let offset = 0
    let group: GroupState | null = null
    let readyRevision = -1
    let applyingUntil = 0
    let startTimer: number | undefined
    let pings = 0
    let reactionKey = 0
    const serverNow = () => Date.now() + offset
    const applying = () => Date.now() < applyingUntil
    const hold = (ms = 400) => {
      applyingUntil = Date.now() + ms
    }
    const raw = (message: object) => {
      if (ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(message))
    }

    const checkReady = () => {
      if (!group?.waiting || readyRevision === group.revision) return
      if (element.readyState >= 3 && !element.seeking) {
        readyRevision = group.revision
        raw({ type: 'ready' })
      }
    }

    const apply = () => {
      if (!group) return
      if (group.item_id !== latest.current.itemId) {
        latest.current.onItem(group.item_id)
        return
      }
      window.clearTimeout(startTimer)
      const now = serverNow()
      const target = expectedPosition(group, now) / 1000
      const off = Math.abs(element.currentTime - target) * 1000
      hold()
      element.playbackRate = 1
      if (!group.playing || group.waiting) {
        if (!element.paused) element.pause()
        if (off > 300) element.currentTime = target
        checkReady()
        return
      }
      if (off > HARD_SEEK_MS) element.currentTime = target
      const delay = startDelay(group, now)
      const start = () => {
        hold()
        if (group?.playing) void element.play().catch(() => {})
      }
      if (delay > 0) {
        if (!element.paused) element.pause()
        startTimer = window.setTimeout(start, delay)
      } else {
        start()
      }
    }

    const ping = () => raw({ type: 'ping', client_ms: Date.now() })
    let pingTimer = window.setTimeout(function next() {
      ping()
      pings += 1
      pingTimer = window.setTimeout(next, pings < 5 ? 1500 : 15_000)
    }, 0)

    ws.onopen = () => setConnected(true)
    ws.onmessage = (event) => {
      let message: ServerMessage
      try {
        message = JSON.parse(String(event.data)) as ServerMessage
      } catch {
        return
      }
      switch (message.type) {
        case 'welcome':
          setMemberId(message.member_id)
          break
        case 'state':
          if (group && message.state.revision < group.revision) return
          group = message.state
          setState(message.state)
          apply()
          break
        case 'pong': {
          samples.push(clockSample(message.client_ms, message.server_ms, Date.now()))
          if (samples.length > 10) samples.shift()
          offset = bestOffset(samples)
          break
        }
        case 'correction':
          hold()
          if (message.seek_to_ms !== null) {
            element.playbackRate = 1
            element.currentTime = message.seek_to_ms / 1000
          } else {
            element.playbackRate = message.rate
          }
          break
        case 'reaction': {
          const key = ++reactionKey
          setReactions((list) => [...list.slice(-5), { key, emoji: message.emoji, name: message.name }])
          window.setTimeout(() => setReactions((list) => list.filter((r) => r.key !== key)), 3500)
          break
        }
        case 'error':
          setError(message.message)
          break
        case 'closed':
          setError(message.reason)
          setClosed(true)
          break
      }
    }
    ws.onclose = () => {
      setConnected(false)
      setClosed(true)
    }

    // The element follows the group, not the other way round.
    const onPlay = () => {
      if (!applying() && group && (!group.playing || group.waiting)) element.pause()
    }
    const onPause = () => {
      if (!applying() && group?.playing && !group.waiting && !element.ended) {
        raw({ type: 'pause', position_ms: Math.round(element.currentTime * 1000) })
      }
    }
    const onWaiting = () => {
      if (group?.playing && !group.waiting && !applying()) raw({ type: 'buffering' })
    }
    const onLoaded = () => apply()
    element.addEventListener('play', onPlay)
    element.addEventListener('pause', onPause)
    element.addEventListener('waiting', onWaiting)
    element.addEventListener('canplay', checkReady)
    element.addEventListener('seeked', checkReady)
    element.addEventListener('loadedmetadata', onLoaded)

    const reportTimer = window.setInterval(() => {
      if (!group?.playing || group.waiting || element.paused) return
      const now = serverNow()
      if (now < group.anchor_ms) return
      raw({ type: 'report', position_ms: Math.round(element.currentTime * 1000), server_ms: Math.round(now) })
    }, 1000)

    return () => {
      window.clearTimeout(startTimer)
      window.clearTimeout(pingTimer)
      window.clearInterval(reportTimer)
      element.removeEventListener('play', onPlay)
      element.removeEventListener('pause', onPause)
      element.removeEventListener('waiting', onWaiting)
      element.removeEventListener('canplay', checkReady)
      element.removeEventListener('seeked', checkReady)
      element.removeEventListener('loadedmetadata', onLoaded)
      ws.onclose = null
      ws.close()
      socket.current = null
      element.playbackRate = 1
    }
  }, [video, groupId])

  const play = useCallback(() => send({ type: 'play' }), [send])
  const pause = useCallback(
    () => send({ type: 'pause', position_ms: Math.round((video.current?.currentTime ?? 0) * 1000) }),
    [send, video],
  )
  const seek = useCallback((seconds: number) => send({ type: 'seek', position_ms: Math.round(Math.max(0, seconds) * 1000) }), [send])
  const setItem = useCallback((id: number) => send({ type: 'set_item', item_id: id }), [send])
  const react = useCallback((emoji: string) => send({ type: 'reaction', emoji }), [send])

  if (!groupId) return null
  return { connected, state, memberId, error, closed, reactions, play, pause, seek, setItem, react }
}
