import { useSyncExternalStore } from 'react'
import type { Schemas } from '@/api/client'
import { readStored, writeStored } from './storage'

/** The signed-in user plus the device token issued by the server. */
export interface Session {
  token: string
  user: Schemas['UserDto']
}

const SESSION_KEY = 'flixnet.session'
const DEVICE_KEY = 'flixnet.device.id'

let current: Session | null = readStored<Session | null>(SESSION_KEY, null)
const listeners = new Set<() => void>()

function emit() {
  for (const listener of listeners) listener()
}

export function getSession(): Session | null {
  return current
}

export function setSession(session: Session | null) {
  current = session
  writeStored(SESSION_KEY, session)
  emit()
}

export function useSession(): Session | null {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener)
      return () => listeners.delete(listener)
    },
    getSession,
  )
}

/** Stable per-browser id, so signing in again replaces this browser's old token. */
export function deviceId(): string {
  const existing = readStored<string | null>(DEVICE_KEY, null)
  if (existing) return existing
  const id = randomId()
  writeStored(DEVICE_KEY, id)
  return id
}

/**
 * 128 random bits as a UUID-shaped string. Not `crypto.randomUUID()`: browsers only
 * offer that on HTTPS or localhost, and Flixnet is often opened as plain
 * `http://<server-ip>:<port>` in the LAN. `getRandomValues` works everywhere.
 */
function randomId(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(16))
  const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('')
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`
}

/** "Chrome · Windows"-style label shown in the device list. */
export function deviceName(): string {
  const ua = navigator.userAgent
  const browser = /Edg\//.test(ua)
    ? 'Edge'
    : /Firefox\//.test(ua)
      ? 'Firefox'
      : /Chrome\//.test(ua)
        ? 'Chrome'
        : /Safari\//.test(ua)
          ? 'Safari'
          : 'Browser'
  const os = /Windows/.test(ua)
    ? 'Windows'
    : /Android/.test(ua)
      ? 'Android'
      : /iPhone|iPad/.test(ua)
        ? 'iOS'
        : /Mac OS X/.test(ua)
          ? 'macOS'
          : /Linux/.test(ua)
            ? 'Linux'
            : 'Unbekannt'
  return `${browser} · ${os}`
}

export function deviceInfo() {
  return { id: deviceId(), name: deviceName(), client: 'Flixnet Web' }
}
