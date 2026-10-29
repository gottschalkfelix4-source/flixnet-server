import { Link } from '@tanstack/react-router'
import clsx from 'clsx'
import { ChevronLeft, ChevronRight, Info, Play, Star } from 'lucide-react'
import { useEffect, useState } from 'react'
import { type ItemCard, useItem } from '@/api/queries/items'
import { formatBadges, formatRuntime } from '@/lib/media'
import { BlurImage } from './BlurImage'

const ROTATE_MS = 9000

/** Full-bleed featured carousel: backdrop, title logo, meta line, badges, synopsis. */
export function Hero({ items }: { items: ItemCard[] }) {
  const [index, setIndex] = useState(0)
  const [paused, setPaused] = useState(false)
  const count = items.length
  const current = items[index % Math.max(count, 1)]
  const detail = useItem(current?.id ?? null)

  useEffect(() => {
    if (paused || count < 2) return
    const timer = setTimeout(() => setIndex((i) => (i + 1) % count), ROTATE_MS)
    return () => clearTimeout(timer)
  }, [index, paused, count])

  if (!current) return null
  const d = detail.data
  const runtime = formatRuntime(d?.runtime_ms ?? current.runtime_ms)
  const badges = d ? formatBadges(d) : []
  const meta = [
    d?.official_rating,
    current.year,
    d?.genres.slice(0, 2).join(' · '),
    current.kind === 'series' && current.child_count > 0
      ? `${current.child_count} ${current.child_count === 1 ? 'Staffel' : 'Staffeln'}`
      : runtime,
  ].filter(Boolean)

  return (
    <section
      className="relative isolate h-[62vh] min-h-[420px] w-full overflow-hidden md:h-[72vh]"
      aria-roledescription="Karussell"
      aria-label="Empfohlen"
      onMouseEnter={() => setPaused(true)}
      onMouseLeave={() => setPaused(false)}
      data-testid="hero"
    >
      {items.map((item, i) => (
        <div
          key={item.id}
          aria-hidden={i !== index}
          className={clsx('absolute inset-0 -z-10 transition-opacity duration-1000', i === index ? 'opacity-100' : 'opacity-0')}
        >
          <BlurImage src={item.backdrop_url} alt="" eager={i === index} className="size-full brightness-90" />
        </div>
      ))}
      {/* Fades into the page at the bottom and towards the text on the left. */}
      <div aria-hidden="true" className="absolute inset-0 -z-10 bg-gradient-to-t from-canvas via-canvas/40 to-transparent" />
      <div aria-hidden="true" className="absolute inset-0 -z-10 bg-gradient-to-r from-canvas/90 via-canvas/40 to-transparent" />

      <div className="flex h-full max-w-3xl flex-col justify-end gap-4 px-6 pb-14 md:px-10">
        {d?.artwork.logo_url ? (
          <img
            src={d.artwork.logo_url}
            alt={current.title}
            className="max-h-28 max-w-[min(26rem,80%)] object-contain object-left drop-shadow-[0_4px_24px_rgba(0,0,0,0.6)] md:max-h-36"
          />
        ) : (
          <h1 className="text-4xl font-extrabold tracking-tight drop-shadow-[0_4px_24px_rgba(0,0,0,0.6)] md:text-6xl">
            {current.title}
          </h1>
        )}
        <div className="flex flex-wrap items-center gap-x-3 gap-y-2 text-sm text-fg/85">
          {current.community_rating != null && (
            <span className="inline-flex items-center gap-1 font-semibold">
              <Star className="size-4 fill-amber-400 text-amber-400" aria-hidden="true" />
              {current.community_rating.toFixed(1)}
            </span>
          )}
          {meta.map((part, i) => (
            <span key={i} className="after:ml-3 after:text-subtle after:content-['·'] last:after:content-none">
              {part}
            </span>
          ))}
          {badges.map((badge) => (
            <span key={badge} className="rounded border border-fg/30 px-1.5 py-0.5 text-[11px] font-bold tracking-wide">
              {badge}
            </span>
          ))}
        </div>
        {d?.overview && <p className="line-clamp-3 max-w-2xl text-base text-fg/80 md:text-lg">{d.overview}</p>}
        <div className="flex flex-wrap items-center gap-3 pt-1">
          <Link
            to="/item/$id"
            params={{ id: String(current.id) }}
            className="inline-flex h-11 items-center gap-2 rounded-full bg-fg px-6 text-sm font-bold text-canvas hover:bg-white"
          >
            <Play className="size-5 fill-current" aria-hidden="true" />
            Abspielen
          </Link>
          <Link
            to="/item/$id"
            params={{ id: String(current.id) }}
            className="inline-flex h-11 items-center gap-2 rounded-full bg-white/15 px-6 text-sm font-bold backdrop-blur hover:bg-white/25"
          >
            <Info className="size-5" aria-hidden="true" />
            Details
          </Link>
        </div>
      </div>

      {count > 1 && (
        <div className="absolute right-6 bottom-6 flex items-center gap-2 md:right-10">
          <button
            type="button"
            aria-label="Vorheriger Titel"
            onClick={() => setIndex((i) => (i - 1 + count) % count)}
            className="rounded-full bg-black/40 p-1.5 backdrop-blur hover:bg-black/60"
          >
            <ChevronLeft className="size-4" />
          </button>
          <span className="min-w-12 text-center text-xs font-semibold text-fg/80 tabular-nums">
            {index + 1} / {count}
          </span>
          <button
            type="button"
            aria-label="Nächster Titel"
            onClick={() => setIndex((i) => (i + 1) % count)}
            className="rounded-full bg-black/40 p-1.5 backdrop-blur hover:bg-black/60"
          >
            <ChevronRight className="size-4" />
          </button>
        </div>
      )}
    </section>
  )
}
