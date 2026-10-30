import { useSyncExternalStore } from 'react'
import { type PlayerState, musicPlayer } from './player'

/** The music player's state; re-renders on every change (position included). */
export function usePlayerState(): PlayerState {
  return useSyncExternalStore(musicPlayer.subscribe, musicPlayer.getState)
}

/** One derived value of the player state; re-renders only when it changes. */
export function usePlayerSelect<T>(select: (state: PlayerState) => T): T {
  return useSyncExternalStore(musicPlayer.subscribe, () => select(musicPlayer.getState()))
}
