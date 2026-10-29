import { useQueries } from '@tanstack/react-query'
import { useWindowVirtualizer } from '@tanstack/react-virtual'
import { forwardRef, useEffect, useImperativeHandle, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { type ItemCard, type ItemsParams, itemsQuery } from '@/api/queries/items'
import { PosterCard, PosterSkeleton } from './PosterCard'

const PAGE = 60
const GAP = 16

export interface VirtualGridHandle {
  /** Scrolls so the item at `offset` (in the current order) is at the top. */
  scrollToOffset: (offset: number) => void
}

interface VirtualGridProps {
  params: ItemsParams
  total: number
  /** Width a card should at least have; columns follow from the container width. */
  minCardWidth?: number
}

/**
 * Poster grid over any number of items: only visible rows are rendered and only
 * the pages they need are fetched (so jumping to "Z" in 50 000 titles is instant).
 */
export const VirtualGrid = forwardRef<VirtualGridHandle, VirtualGridProps>(function VirtualGrid(
  { params, total, minCardWidth = 150 },
  ref,
) {
  const container = useRef<HTMLDivElement>(null)
  const [width, setWidth] = useState(0)
  const [offsetTop, setOffsetTop] = useState(0)

  useLayoutEffect(() => {
    const element = container.current
    if (!element) return
    const observer = new ResizeObserver(([entry]) => {
      setWidth(entry?.contentRect.width ?? 0)
      setOffsetTop(element.getBoundingClientRect().top + window.scrollY)
    })
    observer.observe(element)
    return () => observer.disconnect()
  }, [])

  const columns = Math.max(2, Math.floor((width + GAP) / (minCardWidth + GAP)))
  const cardWidth = width > 0 ? (width - GAP * (columns - 1)) / columns : minCardWidth
  const rowHeight = cardWidth * 1.5 + 52 + GAP
  const rows = Math.ceil(total / columns)

  const virtualizer = useWindowVirtualizer({
    count: rows,
    estimateSize: () => rowHeight,
    overscan: 3,
    scrollMargin: offsetTop,
  })
  // Row height depends on the width; re-measure when it changes.
  useEffect(() => virtualizer.measure(), [rowHeight, virtualizer])

  useImperativeHandle(ref, () => ({
    scrollToOffset: (offset: number) => virtualizer.scrollToIndex(Math.floor(offset / columns), { align: 'start' }),
  }))

  const virtualRows = virtualizer.getVirtualItems()
  const firstItem = (virtualRows[0]?.index ?? 0) * columns
  const lastItem = ((virtualRows.at(-1)?.index ?? 0) + 1) * columns - 1
  const pages = useMemo(() => {
    const out: number[] = []
    for (let p = Math.floor(firstItem / PAGE); p <= Math.floor(Math.min(lastItem, total - 1) / PAGE); p++) out.push(p)
    return out
  }, [firstItem, lastItem, total])

  const results = useQueries({
    queries: pages.map((page) => itemsQuery({ ...params, offset: page * PAGE, limit: PAGE })),
  })
  const loaded = new Map<number, ItemCard>()
  results.forEach((result, i) => {
    const page = pages[i]!
    result.data?.items.forEach((item, j) => loaded.set(page * PAGE + j, item))
  })

  return (
    <div ref={container} className="relative w-full" style={{ height: virtualizer.getTotalSize() }} data-testid="virtual-grid">
      {virtualRows.map((row) => (
        <div
          key={row.key}
          className="absolute inset-x-0 grid"
          style={{
            top: row.start - virtualizer.options.scrollMargin,
            gridTemplateColumns: `repeat(${columns}, minmax(0, 1fr))`,
            gap: GAP,
          }}
        >
          {Array.from({ length: columns }, (_, col) => {
            // Cells are positional: one key namespace per row (item ids could collide with columns).
            const index = row.index * columns + col
            const item = loaded.get(index)
            return (
              <div key={col} className="min-w-0">
                {index >= total ? null : item ? <PosterCard item={item} /> : <PosterSkeleton />}
              </div>
            )
          })}
        </div>
      ))}
    </div>
  )
})
