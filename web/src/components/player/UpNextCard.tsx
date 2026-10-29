import { Play, X } from 'lucide-react'
import type { NextEpisode } from '@/api/queries/player'
import { episodeLabel } from '@/lib/media'

interface UpNextCardProps {
  episode: NextEpisode
  /** Seconds until the next episode starts (the countdown pauses with the video). */
  secondsLeft: number
  onPlayNow: () => void
  onCancel: () => void
}

/** "Nächste Folge in 10 s" near the end of an episode, with play now / cancel. */
export function UpNextCard({ episode, secondsLeft, onPlayNow, onCancel }: UpNextCardProps) {
  const label = [episodeLabel(episode), episode.title].filter(Boolean).join(' · ')
  return (
    <section
      aria-label="Nächste Folge"
      data-testid="up-next"
      className="pointer-events-auto w-[min(22rem,calc(100vw-2rem))] rounded-xl bg-neutral-900/95 p-3 shadow-2xl ring-1 ring-white/10 backdrop-blur"
    >
      <div className="flex gap-3">
        <div className="aspect-video w-28 shrink-0 overflow-hidden rounded-lg bg-white/10">
          {episode.poster_url && <img src={episode.poster_url} alt="" className="size-full object-cover" />}
        </div>
        <div className="min-w-0">
          <p className="text-xs font-bold tracking-wide text-accent uppercase" role="status" data-testid="up-next-countdown">
            Nächste Folge in {secondsLeft} s
          </p>
          <p className="line-clamp-2 text-sm font-semibold" title={label}>
            {label}
          </p>
        </div>
      </div>
      <div className="mt-3 flex gap-2">
        <button
          type="button"
          onClick={onPlayNow}
          className="inline-flex flex-1 items-center justify-center gap-1.5 rounded-full bg-white px-3 py-2 text-sm font-bold whitespace-nowrap text-black hover:bg-white/85"
        >
          <Play className="size-4 fill-current" aria-hidden="true" />
          Jetzt abspielen
        </button>
        <button
          type="button"
          onClick={onCancel}
          className="inline-flex flex-1 items-center justify-center gap-1.5 rounded-full bg-white/15 px-3 py-2 text-sm font-semibold whitespace-nowrap hover:bg-white/25"
        >
          <X className="size-4" aria-hidden="true" />
          Abbrechen
        </button>
      </div>
    </section>
  )
}
