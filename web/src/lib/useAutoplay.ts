import { type RefObject, useEffect, useState } from 'react'

/** Marks a mute the browser forced, so it is not remembered as the user's choice. */
export const POLICY_MUTED = 'data-policy-muted'

export interface Autoplay {
  /** The current source has shown its first frame in motion. */
  started: boolean
  /** The browser only allowed playback without sound. */
  mutedByPolicy: boolean
  /** The browser allowed no playback at all: a tap has to start it. */
  blocked: boolean
  unmute: () => void
  play: () => void
}

/**
 * Starts every new source as soon as it can play. Browsers may refuse playback with
 * sound when the click that opened the player is too long ago (a transcode that takes
 * a while to start); then it starts muted and offers to turn the sound on, and only if
 * even that is refused does it wait for a tap.
 */
export function useAutoplay(video: RefObject<HTMLVideoElement | null>, enabled: boolean): Autoplay {
  const [started, setStarted] = useState(false)
  const [mutedByPolicy, setMutedByPolicy] = useState(false)
  const [blocked, setBlocked] = useState(false)

  useEffect(() => {
    const element = video.current
    if (!element) return
    // One attempt per source: pausing and seeking later must not restart it.
    let attempted = false
    const onEmptied = () => {
      attempted = false
      setStarted(false)
      setBlocked(false)
    }
    const onPlaying = () => {
      // Started some other way (a click, the group): nothing left to start.
      attempted = true
      setStarted(true)
      setBlocked(false)
    }
    const onCanPlay = () => {
      if (!enabled || attempted) return
      attempted = true
      if (!element.paused) return
      element.play().catch((error: unknown) => {
        if (!(error instanceof DOMException) || error.name !== 'NotAllowedError') return
        element.setAttribute(POLICY_MUTED, '')
        element.muted = true
        setMutedByPolicy(true)
        element.play().catch(() => setBlocked(true))
      })
    }
    element.addEventListener('emptied', onEmptied)
    element.addEventListener('playing', onPlaying)
    element.addEventListener('canplay', onCanPlay)
    if (element.readyState >= HTMLMediaElement.HAVE_FUTURE_DATA) onCanPlay()
    return () => {
      element.removeEventListener('emptied', onEmptied)
      element.removeEventListener('playing', onPlaying)
      element.removeEventListener('canplay', onCanPlay)
    }
  }, [video, enabled])

  const unmute = () => {
    if (video.current) restoreSound(video.current)
    setMutedByPolicy(false)
  }

  const play = () => {
    const element = video.current
    if (!element) return
    // A tap counts as permission: with sound, unless the user muted it.
    if (element.hasAttribute(POLICY_MUTED)) unmute()
    void element.play().catch(() => {})
  }

  return { started, mutedByPolicy, blocked, unmute, play }
}

/** Undoes a mute the browser forced. */
function restoreSound(element: HTMLVideoElement) {
  element.removeAttribute(POLICY_MUTED)
  element.muted = false
  if (element.volume === 0) element.volume = 0.5
}
