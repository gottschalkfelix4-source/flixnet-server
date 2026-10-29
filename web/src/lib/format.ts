const relative = new Intl.RelativeTimeFormat('de', { numeric: 'auto' })
const dateTime = new Intl.DateTimeFormat('de', { dateStyle: 'medium', timeStyle: 'short' })

/** "vor 5 Minuten", "gestern", "in 3 Tagen". */
export function relativeTime(ms: number, now = Date.now()): string {
  const diff = (ms - now) / 1000
  const abs = Math.abs(diff)
  if (abs < 60) return relative.format(Math.round(diff), 'second')
  if (abs < 3600) return relative.format(Math.round(diff / 60), 'minute')
  if (abs < 86_400) return relative.format(Math.round(diff / 3600), 'hour')
  return relative.format(Math.round(diff / 86_400), 'day')
}

export function formatDateTime(ms: number): string {
  return dateTime.format(ms)
}

/** Labels for content age limits (FSK steps). */
export const AGE_OPTIONS: { value: number | null; label: string }[] = [
  { value: null, label: 'Keine Beschränkung' },
  { value: 0, label: 'Ab 0 (FSK 0)' },
  { value: 6, label: 'Ab 6 (FSK 6)' },
  { value: 12, label: 'Ab 12 (FSK 12)' },
  { value: 16, label: 'Ab 16 (FSK 16)' },
  { value: 18, label: 'Ab 18 (FSK 18)' },
]

/** Player position: "1:23:45", "4:05". */
export function formatTimecode(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000))
  const hours = Math.floor(total / 3600)
  const minutes = Math.floor((total % 3600) / 60)
  const seconds = String(total % 60).padStart(2, '0')
  return hours > 0 ? `${hours}:${String(minutes).padStart(2, '0')}:${seconds}` : `${minutes}:${seconds}`
}

export function ageLabel(age: number | null | undefined): string {
  return AGE_OPTIONS.find((o) => o.value === (age ?? null))?.label ?? `Ab ${age}`
}
