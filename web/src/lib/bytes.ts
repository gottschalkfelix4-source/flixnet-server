const UNITS = ['B', 'KB', 'MB', 'GB', 'TB']
const number = new Intl.NumberFormat('de', { maximumFractionDigits: 1 })

/** "1,5 GB" (powers of 1024). */
export function formatBytes(bytes: number): string {
  let value = bytes
  let unit = 0
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024
    unit += 1
  }
  return `${number.format(value)} ${UNITS[unit]}`
}

/** Bytes per second as "12,3 Mbit/s". */
export function formatRate(bytesPerSecond: number): string {
  const bits = bytesPerSecond * 8
  if (bits >= 1_000_000) return `${number.format(bits / 1_000_000)} Mbit/s`
  if (bits >= 1000) return `${number.format(bits / 1000)} kbit/s`
  return `${number.format(bits)} bit/s`
}
