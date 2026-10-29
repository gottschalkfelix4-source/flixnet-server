import { Link } from '@tanstack/react-router'
import { Play, Sparkles } from 'lucide-react'
import { type Channel, type Program, formatDay, formatSlot, progress } from '@/api/queries/livetv'
import { Dialog } from '@/components/ui/Dialog'

/** Details of a program, with "Jetzt ansehen" for its channel. */
export function ProgramDialog({
  program,
  channel,
  now,
  onClose,
}: {
  program: Program
  channel: Channel | undefined
  now: number
  onClose: () => void
}) {
  const running = program.start_ms <= now && now < program.end_ms
  const meta = [
    channel?.name,
    formatDay(program.start_ms),
    formatSlot(program),
    program.episode_label,
    program.year ? String(program.year) : null,
    program.rating,
  ].filter(Boolean)

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()} title={program.title} description={meta.join(' · ')}>
      <div className="flex flex-col gap-4" data-testid="program-details">
        {program.subtitle && <p className="text-sm font-semibold">{program.subtitle}</p>}
        {running && (
          <div className="h-1.5 overflow-hidden rounded-full bg-surface-3" aria-label="Fortschritt">
            <div className="h-full bg-accent" style={{ width: `${progress(program, now) * 100}%` }} />
          </div>
        )}
        {program.description && <p className="max-h-60 overflow-y-auto text-sm text-muted">{program.description}</p>}
        {(program.categories.length > 0 || program.is_new) && (
          <div className="flex flex-wrap gap-2">
            {program.is_new && (
              <span className="inline-flex items-center gap-1 rounded-full bg-accent/15 px-2.5 py-0.5 text-xs font-bold text-accent">
                <Sparkles className="size-3" aria-hidden="true" />
                Neu
              </span>
            )}
            {program.categories.map((c) => (
              <span key={c} className="rounded-full bg-surface-3 px-2.5 py-0.5 text-xs font-semibold text-muted">
                {c}
              </span>
            ))}
          </div>
        )}
        {channel && (
          <Link
            to="/live/$channelId"
            params={{ channelId: String(channel.id) }}
            className="inline-flex h-11 items-center justify-center gap-2 self-start rounded-full bg-accent px-6 text-sm font-bold text-accent-fg hover:bg-accent-hover"
          >
            <Play className="size-4 fill-current" aria-hidden="true" />
            {running ? 'Jetzt ansehen' : `${channel.name} ansehen`}
          </Link>
        )}
      </div>
    </Dialog>
  )
}
