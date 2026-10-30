import { ChevronsRight } from 'lucide-react'
import { type RefObject, useEffect, useRef, useState } from 'react'
import { type MediaSegment, usePreferences } from '@/api/queries/segments'

const LABELS: Record<MediaSegment['kind'], string> = {
  intro: 'Intro überspringen',
  recap: 'Rückblick überspringen',
  credits: 'Abspann überspringen',
}

/** The button disappears this close (s) to the segment's end: nothing left to skip. */
const END_MARGIN = 0.5

const key = (s: MediaSegment) => `${s.file_id}:${s.kind}:${s.start_ms}`

/**
 * "Intro überspringen" / "Abspann überspringen": shown while the video is inside a
 * segment of the playing file, jumps to its end. With the profile preference
 * "Intro automatisch überspringen", intros and recaps are skipped without a click,
 * once each (seeking back into one shows the button instead). Self-contained: pass the
 * video element and the source's segments; it positions itself bottom-right.
 */
export function SkipButton({
  videoRef,
  segments,
}: {
  videoRef: RefObject<HTMLVideoElement | null>
  segments: MediaSegment[]
}) {
  const autoSkip = usePreferences().data?.skip_intro_automatically ?? false
  const [active, setActive] = useState<MediaSegment | null>(null)
  const autoSkipped = useRef(new Set<string>())

  useEffect(() => {
    const element = videoRef.current
    if (!element) return
    const update = () => {
      const now = element.currentTime
      const current =
        segments.find((s) => now >= s.start_ms / 1000 && now < s.end_ms / 1000 - END_MARGIN) ?? null
      if (current && autoSkip && current.kind !== 'credits' && !autoSkipped.current.has(key(current))) {
        autoSkipped.current.add(key(current))
        element.currentTime = current.end_ms / 1000
        setActive(null)
        return
      }
      setActive(current)
    }
    update()
    const events = ['timeupdate', 'seeked', 'loadedmetadata'] as const
    for (const name of events) element.addEventListener(name, update)
    return () => {
      for (const name of events) element.removeEventListener(name, update)
    }
  }, [videoRef, segments, autoSkip])

  if (!active) return null
  return (
    <button
      type="button"
      data-testid="skip-button"
      onClick={() => {
        const element = videoRef.current
        if (!element) return
        // Credits usually run to the end of the file: never seek past it.
        const end = active.end_ms / 1000
        element.currentTime = Number.isFinite(element.duration) ? Math.min(end, element.duration) : end
        setActive(null)
      }}
      className="absolute right-6 bottom-24 z-20 flex items-center gap-2 rounded-full border border-white/30 bg-black/70 px-5 py-2.5 text-sm font-semibold text-white shadow-lg backdrop-blur transition hover:bg-white hover:text-black focus-visible:outline-2 focus-visible:outline-accent"
    >
      {LABELS[active.kind]}
      <ChevronsRight className="size-4" aria-hidden="true" />
    </button>
  )
}
