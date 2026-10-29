import type { Chapter, Trickplay } from '@/api/queries/player'

/** One thumbnail as a CSS sprite: the sheet and the tile's offset in it (pixels). */
export interface Tile {
  url: string
  x: number
  y: number
  width: number
  height: number
  sheetWidth: number
  sheetHeight: number
}

/**
 * The thumbnail for a position, as the server lays them out: tile
 * `floor(ms / interval)` (clamped), row-major on sheets of `columns × rows`.
 */
export function tileAt(trickplay: Trickplay, ms: number): Tile | null {
  if (trickplay.thumbnails <= 0 || trickplay.sheets.length === 0) return null
  const perSheet = trickplay.columns * trickplay.rows
  const index = Math.min(Math.max(0, Math.floor(ms / trickplay.interval_ms)), trickplay.thumbnails - 1)
  const sheet = Math.floor(index / perSheet)
  const url = trickplay.sheets[sheet]
  if (!url) return null
  const within = index % perSheet
  return {
    url,
    x: (within % trickplay.columns) * trickplay.tile_width,
    y: Math.floor(within / trickplay.columns) * trickplay.tile_height,
    width: trickplay.tile_width,
    height: trickplay.tile_height,
    sheetWidth: trickplay.columns * trickplay.tile_width,
    sheetHeight: trickplay.rows * trickplay.tile_height,
  }
}

/** The chapter a position falls into. */
export function chapterAt(chapters: Chapter[], ms: number): Chapter | undefined {
  let found: Chapter | undefined
  for (const chapter of chapters) {
    if (chapter.start_ms <= ms) found = chapter
  }
  return found
}

/** "Kapitel 3" when a chapter has no title. */
export function chapterTitle(chapter: Chapter): string {
  return chapter.title?.trim() || `Kapitel ${chapter.index + 1}`
}
