import type { ItemCard, ItemDetail } from '@/api/queries/items'

/** "1 Std. 52 Min." / "47 Min." */
export function formatRuntime(ms: number | null | undefined): string | null {
  if (!ms || ms <= 0) return null
  const minutes = Math.round(ms / 60_000)
  if (minutes < 60) return `${minutes} Min.`
  const hours = Math.floor(minutes / 60)
  const rest = minutes % 60
  return rest ? `${hours} Std. ${rest} Min.` : `${hours} Std.`
}

/** "S1 · F3" or "S1 · F3–4" for episodes; null otherwise. */
export function episodeLabel(item: Pick<ItemCard, 'kind' | 'parent_index_number' | 'index_number' | 'index_end'>): string | null {
  if (item.kind !== 'episode' || item.index_number == null) return null
  const season = item.parent_index_number != null ? `S${item.parent_index_number} · ` : ''
  const end = item.index_end != null && item.index_end > item.index_number ? `–${item.index_end}` : ''
  return `${season}F${item.index_number}${end}`
}

/** Technical badges from the best main file: 4K, HDR10/Dolby Vision, 5.1/7.1, Atmos. */
export function formatBadges(item: ItemDetail): string[] {
  const main = item.files.find((f) => f.role === 'main')
  if (!main) return []
  const badges: string[] = []
  const video = main.video
  if (video?.height) {
    if (video.height >= 2000 || (video.width ?? 0) >= 3800) badges.push('4K')
    else if (video.height >= 1000) badges.push('HD')
  }
  if (video?.hdr === 'dovi') badges.push('Dolby Vision')
  else if (video?.hdr === 'hdr10') badges.push('HDR10')
  else if (video?.hdr === 'hlg') badges.push('HLG')
  const bestAudio = [...main.audio].sort((a, b) => (b.channels ?? 0) - (a.channels ?? 0))[0]
  if (bestAudio?.channels && bestAudio.channels >= 8) badges.push('7.1')
  else if (bestAudio?.channels && bestAudio.channels >= 6) badges.push('5.1')
  if (main.audio.some((a) => a.codec === 'truehd' && (a.title ?? '').toLowerCase().includes('atmos'))) {
    badges.push('Atmos')
  }
  return badges
}

/** Year range for series ("2017–2020" would need an end date; start year for now). */
export function yearLabel(item: Pick<ItemCard, 'year'>): string | null {
  return item.year ? String(item.year) : null
}

const LANGUAGES: Record<string, string> = {
  deu: 'Deutsch',
  ger: 'Deutsch',
  eng: 'Englisch',
  fra: 'Französisch',
  fre: 'Französisch',
  spa: 'Spanisch',
  ita: 'Italienisch',
  jpn: 'Japanisch',
  kor: 'Koreanisch',
  nld: 'Niederländisch',
  pol: 'Polnisch',
  rus: 'Russisch',
  tur: 'Türkisch',
}

export function languageName(code: string | null | undefined): string {
  if (!code) return 'Unbekannt'
  return LANGUAGES[code] ?? code.toUpperCase()
}
