/**
 * Pure SyncPlay helpers: clock sync and where the group is right now. The server
 * owns the state machine (crates/flixnet-api/src/syncplay/state.rs); clients follow.
 */

export interface GroupState {
  group_id: string
  item_id: number
  playing: boolean
  position_ms: number
  anchor_ms: number
  server_ms: number
  waiting: boolean
  members: { id: number; user_id: number; name: string; ready: boolean }[]
  revision: number
}

/** One ping/pong round trip. */
export interface ClockSample {
  /** server time − local time (ms). */
  offset: number
  rtt: number
}

/** Offset estimate from a pong: the server answered halfway through the round trip. */
export function clockSample(sentAt: number, serverMs: number, receivedAt: number): ClockSample {
  const rtt = Math.max(0, receivedAt - sentAt)
  return { offset: serverMs + rtt / 2 - receivedAt, rtt }
}

/** The sample with the shortest round trip is the most precise. */
export function bestOffset(samples: ClockSample[]): number {
  if (samples.length === 0) return 0
  return samples.reduce((best, s) => (s.rtt < best.rtt ? s : best)).offset
}

/** Where the group is at server time `serverNow` (ms). */
export function expectedPosition(state: GroupState, serverNow: number): number {
  if (!state.playing) return state.position_ms
  return state.position_ms + Math.max(0, serverNow - state.anchor_ms)
}

/** Milliseconds until a scheduled start (0 when it already passed or not playing). */
export function startDelay(state: GroupState, serverNow: number): number {
  return state.playing ? Math.max(0, state.anchor_ms - serverNow) : 0
}

/** Far enough off to jump instead of waiting for the server's rate nudges. */
export const HARD_SEEK_MS = 1500

/** WebSocket URL of a group, relative to the page. */
export function socketUrl(location: { protocol: string; host: string }, groupId: string): string {
  const scheme = location.protocol === 'https:' ? 'wss:' : 'ws:'
  return `${scheme}//${location.host}/api/v1/syncplay/groups/${encodeURIComponent(groupId)}/ws`
}

/** Reactions the server relays. */
export const REACTIONS = ['👍', '😂', '😮', '😢', '❤️', '🔥', '👏', '🍿'] as const
