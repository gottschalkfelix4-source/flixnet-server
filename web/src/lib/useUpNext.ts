import { type RefObject, useEffect, useRef, useState } from 'react'

/** The card shows this long before the end (until credits segments say better). */
export const UP_NEXT_BEFORE_END_S = 30
/** Countdown on the card, in seconds of playback (it pauses with the video). */
export const UP_NEXT_COUNTDOWN_S = 10

export interface UpNext {
  visible: boolean
  /** Whole seconds left on the countdown. */
  secondsLeft: number
  cancel: () => void
}

/**
 * "Nächste Folge" near the end: from `UP_NEXT_BEFORE_END_S` before the end, but not in
 * the first half (or from `startsAt`, e.g. where the credits begin), the card shows and
 * counts down while the video plays; at zero, or when the video ends, `onAdvance` runs once. A seek starts
 * the countdown over (the viewer is busy), seeking back before the start hides the
 * card again, and cancelling stops it for this item.
 */
export function useUpNext(
  video: RefObject<HTMLVideoElement | null>,
  enabled: boolean,
  onAdvance: () => void,
  startsAt?: number,
): UpNext {
  const [shown, setShown] = useState(false)
  const [secondsLeft, setSecondsLeft] = useState(UP_NEXT_COUNTDOWN_S)
  const [cancelled, setCancelled] = useState(false)
  const advance = useRef(onAdvance)
  useEffect(() => {
    advance.current = onAdvance
  })

  useEffect(() => {
    const element = video.current
    if (!element || !enabled || cancelled) return
    let active = false
    let played = 0
    let last = 0
    let done = false
    const go = () => {
      if (done) return
      done = true
      advance.current()
    }
    const onTime = () => {
      const duration = element.duration
      if (!Number.isFinite(duration) || duration <= 0) return
      // Never in the first half (short episodes).
      const from = startsAt ?? Math.max(duration / 2, duration - UP_NEXT_BEFORE_END_S)
      const time = element.currentTime
      if (time < from) {
        if (active) {
          active = false
          setShown(false)
        }
        return
      }
      if (!active) {
        active = true
        played = 0
        last = time
        setShown(true)
        setSecondsLeft(UP_NEXT_COUNTDOWN_S)
        return
      }
      // Only time that was played counts; jumps are seeks.
      const step = time - last
      last = time
      if (step > 0 && step < 1.5) played += step
      setSecondsLeft(Math.max(0, Math.ceil(UP_NEXT_COUNTDOWN_S - played)))
      if (played >= UP_NEXT_COUNTDOWN_S) go()
    }
    const onSeeked = () => {
      played = 0
      last = element.currentTime
      setSecondsLeft(UP_NEXT_COUNTDOWN_S)
    }
    element.addEventListener('timeupdate', onTime)
    element.addEventListener('seeked', onSeeked)
    element.addEventListener('ended', go)
    return () => {
      element.removeEventListener('timeupdate', onTime)
      element.removeEventListener('seeked', onSeeked)
      element.removeEventListener('ended', go)
    }
  }, [video, enabled, cancelled, startsAt])

  return { visible: enabled && !cancelled && shown, secondsLeft, cancel: () => setCancelled(true) }
}
