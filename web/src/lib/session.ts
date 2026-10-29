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
  const id = crypto.randomUUID()
  writeStored(DEVICE_KEY, id)
  return id
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
