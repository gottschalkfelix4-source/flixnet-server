import clsx from 'clsx'
import { Play, RotateCcw, X } from 'lucide-react'
import { useState } from 'react'
import { useCancelJob, useJobs, useRetryJob, useRunTask, useTasks } from '@/api/queries/jobs'
import { Button } from '@/components/ui/Button'
import { Section } from '@/components/ui/Section'
import { formatDateTime, relativeTime } from '@/lib/format'
import { KIND_LABELS } from '@/lib/jobs'


const STATE_STYLES: Record<string, { label: string; className: string }> = {
  queued: { label: 'Wartet', className: 'bg-surface-3 text-muted' },
  running: { label: 'Läuft', className: 'bg-accent/15 text-accent' },
  done: { label: 'Erledigt', className: 'bg-emerald-500/15 text-emerald-400' },
  failed: { label: 'Fehlgeschlagen', className: 'bg-red-500/15 text-red-400' },
  cancelled: { label: 'Abgebrochen', className: 'bg-surface-3 text-subtle' },
}

function interval(ms: number): string {
  const minutes = Math.round(ms / 60_000)
  if (minutes < 60) return `alle ${minutes} Min.`
  const hours = Math.round(minutes / 60)
  if (hours < 24) return hours === 1 ? 'stündlich' : `alle ${hours} Std.`
  const days = Math.round(hours / 24)
  return days === 1 ? 'täglich' : `alle ${days} Tage`
}

export function JobsSettings() {
  const tasks = useTasks()
  const jobs = useJobs()
  const runTask = useRunTask()
  const cancel = useCancelJob()
  const retry = useRetryJob()
  const [now] = useState(() => Date.now())

  return (
    <div className="flex flex-col gap-6">
      <Section title="Geplante Aufgaben" description="Laufen automatisch im Hintergrund.">
        <ul className="divide-y divide-line/60">
          {tasks.data?.map((task) => (
            <li key={task.name} className="flex flex-wrap items-center gap-4 py-3">
              <div className="min-w-0 flex-1">
                <p className="text-sm font-semibold">{task.description}</p>
                <p className="text-xs text-subtle">
                  {task.enabled ? interval(task.interval_ms) : 'deaktiviert'}
                  {task.last_run ? ` · zuletzt ${relativeTime(task.last_run, now)}` : ' · noch nie'}
                </p>
              </div>
              <Button
                variant="ghost"
                className="h-9 px-4"
                onClick={() => runTask.mutate(task.name)}
                loading={runTask.isPending && runTask.variables === task.name}
              >
                <Play className="size-4" aria-hidden="true" />
                Jetzt ausführen
              </Button>
            </li>
          ))}
        </ul>
      </Section>

      <Section title="Aktivität" description="Laufende, wartende und zuletzt erledigte Hintergrundaufgaben.">
        {jobs.data?.length === 0 && <p className="text-sm text-muted">Noch keine Aufgaben.</p>}
        <ul className="divide-y divide-line/60" data-testid="job-list">
          {jobs.data?.map((job) => {
            const style = STATE_STYLES[job.state] ?? STATE_STYLES.queued!
            return (
              <li key={job.id} className="flex flex-wrap items-center gap-4 py-3">
                <div className="min-w-0 flex-1">
                  <p className="flex items-center gap-2 text-sm font-semibold">
                    {KIND_LABELS[job.kind] ?? job.kind}
                    <span className={clsx('rounded-full px-2 py-0.5 text-xs font-bold', style.className)}>
                      {style.label}
                    </span>
                  </p>
                  <p className="text-xs text-subtle">
                    #{job.id} · {job.pool} · {formatDateTime(job.finished_at ?? job.started_at ?? job.created_at)}
                    {job.attempts > 1 && ` · Versuch ${job.attempts}/${job.max_attempts}`}
                  </p>
                  {job.state === 'running' && job.progress != null && (
                    <div className="mt-2 h-1 w-48 overflow-hidden rounded-full bg-surface-3">
                      <div className="h-full bg-accent" style={{ width: `${Math.round(job.progress * 100)}%` }} />
                    </div>
                  )}
                  {job.error && <p className="mt-1 text-xs text-red-400">{job.error}</p>}
                </div>
                {(job.state === 'queued' || job.state === 'running') && (
                  <Button variant="ghost" className="h-9 px-3" aria-label="Abbrechen" onClick={() => cancel.mutate(job.id)}>
                    <X className="size-4" />
                  </Button>
                )}
                {(job.state === 'failed' || job.state === 'cancelled') && (
                  <Button variant="ghost" className="h-9 px-3" aria-label="Erneut versuchen" onClick={() => retry.mutate(job.id)}>
                    <RotateCcw className="size-4" />
                  </Button>
                )}
              </li>
            )
          })}
        </ul>
      </Section>
    </div>
  )
}
