import { RotateCcw } from 'lucide-react'
import { useEffect, useState } from 'react'
import { formatTimecode } from '@/lib/format'

/**
 * "Fortgesetzt bei 1:23:45 · Von vorne": the player resumes right away and offers
 * starting over for a few seconds (no blocking question before playback).
 */
export function ResumeToast({ positionMs, onRestart }: { positionMs: number; onRestart: () => void }) {
  const [visible, setVisible] = useState(true)
  useEffect(() => {
    const timer = window.setTimeout(() => setVisible(false), 8000)
    return () => window.clearTimeout(timer)
  }, [])
  if (!visible) return null

  return (
    <div
      role="status"
      data-testid="resume-toast"
      className="absolute bottom-24 left-1/2 z-10 flex -translate-x-1/2 items-center gap-3 rounded-full bg-black/80 py-2 pr-2 pl-5 text-sm shadow-lg backdrop-blur"
    >
      <span>
        Fortgesetzt bei <span className="font-semibold tabular-nums">{formatTimecode(positionMs)}</span>
      </span>
      <button
        type="button"
        onClick={() => {
          setVisible(false)
          onRestart()
        }}
        className="inline-flex items-center gap-1.5 rounded-full bg-white/15 px-3 py-1.5 font-semibold hover:bg-white/25"
      >
        <RotateCcw className="size-4" aria-hidden="true" />
        Von vorne
      </button>
    </div>
  )
}
