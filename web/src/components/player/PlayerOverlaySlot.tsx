import clsx from 'clsx'
import type { ReactNode } from 'react'

/**
 * Bottom-right area above the control bar for cards and buttons that appear during
 * playback: "Nächste Folge", and skip buttons for intro/credits segments (dropped in
 * by the segment feature). Stays clickable while the controls are hidden; moves up
 * when they show, so nothing covers the seek bar.
 */
export function PlayerOverlaySlot({ controlsVisible, children }: { controlsVisible: boolean; children: ReactNode }) {
  return (
    <div
      data-testid="player-overlay-slot"
      className={clsx(
        'pointer-events-none absolute right-4 z-20 flex flex-col items-end gap-3 transition-[bottom] duration-200 sm:right-8',
        controlsVisible ? 'bottom-32' : 'bottom-8',
      )}
    >
      {children}
    </div>
  )
}
