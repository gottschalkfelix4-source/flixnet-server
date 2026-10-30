import { Link } from '@tanstack/react-router'
import { CircleDot, ListVideo, Play, Sparkles, TriangleAlert } from 'lucide-react'
import { useState } from 'react'
import { ApiError } from '@/api/client'
import {
  type Channel,
  type Program,
  RECORDING_STATUS_LABELS,
  formatDay,
  formatSlot,
  progress,
  useCreateRecording,
  useCreateRecordingRule,
  useDeleteRecording,
} from '@/api/queries/livetv'
import { Button } from '@/components/ui/Button'
import { Dialog } from '@/components/ui/Dialog'
import { Checkbox, SelectField } from '@/components/ui/Section'

/** Details of a program, with "Jetzt ansehen", "Aufnehmen" and "Serie aufnehmen". */
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
  const over = program.end_ms <= now
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
        <div className="flex flex-wrap items-center gap-3">
          {channel && !program.restricted && (
            <Link
              to="/live/$channelId"
              params={{ channelId: String(channel.id) }}
              className="inline-flex h-11 items-center justify-center gap-2 rounded-full bg-accent px-6 text-sm font-bold text-accent-fg hover:bg-accent-hover"
            >
              <Play className="size-4 fill-current" aria-hidden="true" />
              {running ? 'Jetzt ansehen' : `${channel.name} ansehen`}
            </Link>
          )}
        </div>
        {!program.restricted && !over && <RecordActions program={program} channel={channel} />}
      </div>
    </Dialog>
  )
}

function errorText(error: unknown): string | null {
  if (!error) return null
  return error instanceof ApiError ? error.message : 'Das hat nicht geklappt.'
}

/** Record this broadcast or the whole series, or cancel what is planned. */
function RecordActions({ program, channel }: { program: Program; channel: Channel | undefined }) {
  const record = useCreateRecording()
  const series = useCreateRecordingRule()
  const cancel = useDeleteRecording()
  const [seriesForm, setSeriesForm] = useState(false)
  const [newOnly, setNewOnly] = useState(true)
  const [channelOnly, setChannelOnly] = useState(true)
  const [keep, setKeep] = useState('')

  // What the guide said, updated by what was done here.
  const recording = cancel.isSuccess
    ? null
    : record.data
      ? { id: record.data.id, status: record.data.status, series: false, conflict: record.data.conflict }
      : (program.recording ?? null)
  const seriesDone = series.data
  const error = errorText(record.error ?? series.error ?? cancel.error)

  return (
    <div className="flex flex-col gap-3 border-t border-line/60 pt-4" data-testid="record-actions">
      {recording ? (
        <div className="flex flex-wrap items-center gap-3">
          <span
            className="inline-flex items-center gap-1.5 rounded-full bg-red-600/15 px-3 py-1 text-sm font-bold text-red-400"
            data-testid="recording-state"
          >
            <CircleDot className="size-4" aria-hidden="true" />
            {recording.status === 'scheduled' ? 'Aufnahme geplant' : RECORDING_STATUS_LABELS[recording.status]}
            {recording.series ? ' (Serie)' : ''}
          </span>
          {recording.conflict && (
            <span className="inline-flex items-center gap-1 text-sm text-amber-400" data-testid="recording-conflict">
              <TriangleAlert className="size-4" aria-hidden="true" />
              Konflikt: kein Tuner frei
            </span>
          )}
          {recording.status === 'scheduled' && (
            <Button variant="ghost" loading={cancel.isPending} onClick={() => cancel.mutate(recording.id)}>
              {recording.series ? 'Diese Folge überspringen' : 'Aufnahme abbrechen'}
            </Button>
          )}
        </div>
      ) : (
        <div className="flex flex-wrap items-center gap-3">
          <Button variant="primary" loading={record.isPending} onClick={() => record.mutate(program.id)}>
            <CircleDot className="size-4 text-red-500" aria-hidden="true" />
            Aufnehmen
          </Button>
          {!seriesForm && !seriesDone && (
            <Button variant="ghost" onClick={() => setSeriesForm(true)}>
              <ListVideo className="size-4" aria-hidden="true" />
              Serie aufnehmen
            </Button>
          )}
        </div>
      )}
      {seriesDone ? (
        <p className="text-sm text-muted" data-testid="series-scheduled">
          Serie „{seriesDone.title}“ wird aufgenommen: {seriesDone.scheduled}{' '}
          {seriesDone.scheduled === 1 ? 'Sendung' : 'Sendungen'} geplant
          {seriesDone.conflicts > 0 ? `, ${seriesDone.conflicts} mit Konflikt` : ''}.
        </p>
      ) : (
        seriesForm &&
        !recording && (
          <form
            className="flex flex-col gap-3 rounded-xl bg-surface-2 p-4"
            onSubmit={(event) => {
              event.preventDefault()
              series.mutate({
                program_id: program.id,
                new_only: newOnly,
                channel_id: channelOnly ? program.channel_id : null,
                keep_last: keep ? Number(keep) : null,
              })
            }}
          >
            <Checkbox
              label="Nur neue Folgen"
              hint="Bereits aufgenommene Folgen und Wiederholungen auslassen."
              checked={newOnly}
              onChange={setNewOnly}
            />
            <Checkbox label={`Nur auf ${channel?.name ?? 'diesem Sender'}`} checked={channelOnly} onChange={setChannelOnly} />
            <SelectField
              label="Behalten"
              value={keep}
              onChange={setKeep}
              options={[
                { value: '', label: 'Alle Aufnahmen' },
                { value: '3', label: 'Die letzten 3' },
                { value: '5', label: 'Die letzten 5' },
                { value: '10', label: 'Die letzten 10' },
              ]}
            />
            <div className="flex gap-3">
              <Button type="submit" variant="accent" loading={series.isPending}>
                Serie aufnehmen
              </Button>
              <Button type="button" variant="ghost" onClick={() => setSeriesForm(false)}>
                Abbrechen
              </Button>
            </div>
          </form>
        )
      )}
      {error && (
        <p role="alert" className="text-sm text-red-400">
          {error}
        </p>
      )}
    </div>
  )
}
