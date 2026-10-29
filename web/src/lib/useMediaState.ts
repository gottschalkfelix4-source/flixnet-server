import { type RefObject, useEffect, useState } from 'react'

export interface MediaState {
  /** Seconds. */
  currentTime: number
  /** Seconds; 0 while unknown. */
  duration: number
  /** End of the buffered range around the current position (seconds). */
  bufferedEnd: number
  paused: boolean
  ended: boolean
  /** Waiting for data (spinner). */
  waiting: boolean
  volume: number
  muted: boolean
  playbackRate: number
}

const INITIAL: MediaState = {
  currentTime: 0,
  duration: 0,
  bufferedEnd: 0,
  paused: true,
  ended: false,
  waiting: false,
  volume: 1,
  muted: false,
  playbackRate: 1,
}

const EVENTS = [
  'timeupdate',
  'durationchange',
  'loadedmetadata',
  'progress',
  'play',
  'pause',
  'playing',
  'waiting',
  'seeking',
  'seeked',
  'ended',
  'volumechange',
  'ratechange',
  'emptied',
  'canplay',
] as const

function read(video: HTMLVideoElement, waiting: boolean): MediaState {
  const time = video.currentTime
  let bufferedEnd = 0
  for (let i = 0; i < video.buffered.length; i++) {
    if (video.buffered.start(i) <= time + 0.5 && video.buffered.end(i) >= time) bufferedEnd = video.buffered.end(i)
  }
  return {
    currentTime: time,
    duration: Number.isFinite(video.duration) ? video.duration : 0,
    bufferedEnd,
    paused: video.paused,
    ended: video.ended,
    waiting,
    volume: video.volume,
    muted: video.muted,
    playbackRate: video.playbackRate,
  }
}

/** The video element's playback state as React state, updated from its events. */
export function useMediaState(video: RefObject<HTMLVideoElement | null>): MediaState {
  const [state, setState] = useState<MediaState>(INITIAL)

  useEffect(() => {
    const element = video.current
    if (!element) return
    let waiting = false
    const update = (event?: Event) => {
      if (event?.type === 'waiting' || event?.type === 'seeking') waiting = true
      else if (event?.type === 'playing' || event?.type === 'canplay' || event?.type === 'seeked' || event?.type === 'pause')
        waiting = element.seeking
      setState(read(element, waiting))
    }
    for (const name of EVENTS) element.addEventListener(name, update)
    update()
    return () => {
      for (const name of EVENTS) element.removeEventListener(name, update)
    }
  }, [video])

  return state
}
