import { type RefObject, useLayoutEffect, useState } from 'react'

/**
 * Categorical series colors on the dark surface, in fixed order (validated as a set:
 * lightness band, chroma, CVD and normal-vision separation, ≥ 3:1 contrast).
 */
export const SERIES = {
  movies: '#3987e5',
  tv: '#d95926',
  music: '#199e70',
  other: '#c98500',
  local: '#3987e5',
  remote: '#d95926',
} as const

/** Width of an element, following resizes (charts draw in real pixels, not a stretched viewBox). */
export function useWidth(ref: RefObject<HTMLElement | null>): number {
  const [width, setWidth] = useState(0)
  useLayoutEffect(() => {
    const element = ref.current
    if (!element) return
    const observer = new ResizeObserver(([entry]) => setWidth(Math.floor(entry?.contentRect.width ?? 0)))
    observer.observe(element)
    return () => observer.disconnect()
  }, [ref])
  return width
}

/** "Nice" upper bound and ~4 ticks for an axis starting at 0. */
export function niceTicks(max: number): number[] {
  if (max <= 0) return [0, 1]
  const rough = max / 4
  const power = 10 ** Math.floor(Math.log10(rough))
  const step = [1, 2, 2.5, 5, 10].map((m) => m * power).find((s) => s >= rough) ?? rough
  const ticks: number[] = []
  for (let v = 0; v <= max + step * 0.001; v += step) ticks.push(v)
  if (ticks[ticks.length - 1]! < max) ticks.push(ticks[ticks.length - 1]! + step)
  return ticks
}
