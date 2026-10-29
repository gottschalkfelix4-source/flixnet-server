import useEmblaCarousel from 'embla-carousel-react'
import { ChevronLeft, ChevronRight } from 'lucide-react'
import { type ReactNode, useCallback, useEffect, useState } from 'react'
import type { ItemCard } from '@/api/queries/items'
import { PosterCard, PosterSkeleton } from './PosterCard'

interface RowProps {
  title: string
  items: ItemCard[] | undefined
  loading?: boolean
  wide?: boolean
  /** Shown next to the title (e.g. a link to the full list). */
  action?: ReactNode
}

/** A titled, horizontally scrolling row of cards with arrow buttons. */
export function Row({ title, items, loading = false, wide = false, action }: RowProps) {
  const [viewport, embla] = useEmblaCarousel({ align: 'start', dragFree: true, containScroll: 'trimSnaps' })
  const [canPrev, setCanPrev] = useState(false)
  const [canNext, setCanNext] = useState(false)

  const update = useCallback(() => {
    if (!embla) return
    setCanPrev(embla.canScrollPrev())
    setCanNext(embla.canScrollNext())
  }, [embla])

  useEffect(() => {
    if (!embla) return
    embla.on('select', update).on('reInit', update).on('scroll', update)
    // Initial state once Embla has measured.
    queueMicrotask(update)
    return () => {
      embla.off('select', update).off('reInit', update).off('scroll', update)
    }
  }, [embla, update])

  if (!loading && (!items || items.length === 0)) return null
  const cardWidth = wide ? 'w-72 md:w-80' : 'w-36 md:w-44'

  return (
    <section className="mb-8" aria-label={title}>
      <div className="mb-3 flex items-center justify-between px-6 md:px-10">
        <div className="flex min-w-0 items-baseline gap-3">
          <h2 className="truncate text-lg font-bold tracking-tight md:text-xl">{title}</h2>
          {action}
        </div>
        <div className="hidden gap-1 md:flex">
          <button
            type="button"
            aria-label={`${title}: zurück`}
            disabled={!canPrev}
            onClick={() => embla?.scrollPrev()}
            className="rounded-full p-1.5 text-muted hover:bg-surface-2 hover:text-fg disabled:opacity-30"
          >
            <ChevronLeft className="size-5" />
          </button>
          <button
            type="button"
            aria-label={`${title}: weiter`}
            disabled={!canNext}
            onClick={() => embla?.scrollNext()}
            className="rounded-full p-1.5 text-muted hover:bg-surface-2 hover:text-fg disabled:opacity-30"
          >
            <ChevronRight className="size-5" />
          </button>
        </div>
      </div>
      <div ref={viewport} className="overflow-hidden px-6 md:px-10">
        <div className="flex gap-4">
          {loading && !items
            ? Array.from({ length: 8 }, (_, i) => (
                <div key={i} className={`${cardWidth} shrink-0`}>
                  <PosterSkeleton wide={wide} />
                </div>
              ))
            : items?.map((item) => (
                <div key={item.id} className={`${cardWidth} shrink-0`}>
                  <PosterCard item={item} wide={wide} />
                </div>
              ))}
        </div>
      </div>
    </section>
  )
}
