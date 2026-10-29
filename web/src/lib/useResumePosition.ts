import { useState } from 'react'
import type { Schemas } from '@/api/client'

type Source = Schemas['MediaSourceDto']

/** Parts of an item on one timeline (multi-part movies play their parts in a row). */
export interface Timeline {
  /** Start of each part on the item's timeline (ms). */
  offsets: number[]
  /** Duration of all parts together, if every part's duration is known. */
  totalMs: number | null
}

export function partTimeline(sources: Source[]): Timeline {
  const offsets: number[] = []
  let sum = 0
  let known = sources.length > 0
  for (const source of sources) {
    offsets.push(sum)
    if (source.duration_ms) sum += source.duration_ms
    else known = false
  }
  return { offsets, totalMs: known ? sum : null }
}

/** The part containing a position, and the position inside that part. */
export function locate(timeline: Timeline, positionMs: number): { part: number; withinMs: number } {
  let part = 0
  timeline.offsets.forEach((offset, i) => {
    if (offset <= positionMs) part = i
  })
  return { part, withinMs: Math.max(0, positionMs - (timeline.offsets[part] ?? 0)) }
}

/**
 * Where playback starts (ms on the item's timeline): an explicit `?start=` (0 = from the
 * beginning) or the saved resume point; `null` until the item is loaded. Fixed once
 * known, so progress reports moving the resume point never restart the video.
 */
export function useStartPosition(explicitMs: number | undefined, resumeMs: number | undefined): number | null {
  const [fixed, setFixed] = useState<number | null>(null)
  const value = fixed ?? explicitMs ?? resumeMs ?? null
  if (fixed === null && value !== null) setFixed(value)
  return value
}
