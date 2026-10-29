import { Pencil, RefreshCw, Trash2 } from 'lucide-react'
import { type FormEvent, useState } from 'react'
import {
  type MediaSegment,
  type SegmentKind,
  useAnalyzeSegments,
  useDeleteSegment,
  useMediaSegments,
  useSetSegment,
} from '@/api/queries/segments'
import { FormError } from '@/components/layout/AuthLayout'
import { Button } from '@/components/ui/Button'
import { formatTimecode } from '@/lib/format'
import { useSession } from '@/lib/session'

const KINDS: { kind: SegmentKind; label: string }[] = [
  { kind: 'intro', label: 'Intro' },
  { kind: 'credits', label: 'Abspann' },
]

/** "1:23" / "83" / "1:02:03" → ms; `null` when unreadable. */
function parseTimecode(text: string): number | null {
  const parts = text.trim().split(':')
  if (parts.length === 0 || parts.length > 3 || parts.some((p) => !/^\d+(\.\d+)?$/.test(p))) return null
  return Math.round(parts.reduce((total, part) => total * 60 + Number(part), 0) * 1000)
}

/**
 * Detected intro and credits of an episode (administrators): check them, correct them
 * by hand, or analyze the season again.
 */
export function SegmentsPanel({ itemId }: { itemId: number }) {
  const isAdmin = useSession()?.user.is_admin ?? false
  const segments = useMediaSegments(itemId, isAdmin)
  const analyze = useAnalyzeSegments(itemId)
  const [editing, setEditing] = useState<SegmentKind | null>(null)
  if (!isAdmin) return null
  // Episodes normally have one file; the panel shows the first one's segments.
  const fileId = segments.data?.[0]?.file_id
  const of = (kind: SegmentKind) => segments.data?.find((s) => s.kind === kind && s.file_id === (fileId ?? s.file_id))

  return (
    <section className="mb-10 px-6 md:px-10" aria-label="Intro und Abspann" data-testid="segments-panel">
      <div className="rounded-[var(--radius-card)] border border-line/60 bg-surface/60 p-4 text-sm">
        <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
          <p className="font-bold">Intro &amp; Abspann</p>
          <Button variant="ghost" className="h-9 px-4" loading={analyze.isPending} onClick={() => analyze.mutate()}>
            <RefreshCw className="size-4" aria-hidden="true" />
            Staffel analysieren
          </Button>
        </div>
        {analyze.isSuccess && (
          <p role="status" className="mb-2 text-xs text-subtle">
            {analyze.data.seasons > 0 ? 'Analyse eingeplant.' : 'Die Analyse läuft bereits.'}
          </p>
        )}
        <ul className="divide-y divide-line/60">
          {KINDS.map(({ kind, label }) => (
            <SegmentRow
              key={kind}
              itemId={itemId}
              kind={kind}
              label={label}
              segment={of(kind)}
              editing={editing === kind}
              onEdit={(on) => setEditing(on ? kind : null)}
            />
          ))}
        </ul>
      </div>
    </section>
  )
}

function SegmentRow({
  itemId,
  kind,
  label,
  segment,
  editing,
  onEdit,
}: {
  itemId: number
  kind: SegmentKind
  label: string
  segment: MediaSegment | undefined
  editing: boolean
  onEdit: (on: boolean) => void
}) {
  const set = useSetSegment(itemId)
  const remove = useDeleteSegment(itemId)
  const [start, setStart] = useState('')
  const [end, setEnd] = useState('')
  const [invalid, setInvalid] = useState(false)

  function open() {
    setStart(segment ? formatTimecode(segment.start_ms) : '')
    setEnd(segment ? formatTimecode(segment.end_ms) : '')
    setInvalid(false)
    onEdit(true)
  }

  function submit(event: FormEvent) {
    event.preventDefault()
    const start_ms = parseTimecode(start)
    const end_ms = parseTimecode(end)
    if (start_ms === null || end_ms === null || end_ms <= start_ms) {
      setInvalid(true)
      return
    }
    set.mutate({ kind, start_ms, end_ms }, { onSuccess: () => onEdit(false) })
  }

  const source = segment?.source === 'manual' ? 'manuell' : `erkannt (${Math.round((segment?.confidence ?? 0) * 100)} %)`
  return (
    <li className="py-2" data-testid={`segment-${kind}`}>
      <div className="flex flex-wrap items-center gap-3">
        <span className="w-20 font-semibold">{label}</span>
        <span className="flex-1 text-muted">
          {segment ? `${formatTimecode(segment.start_ms)} – ${formatTimecode(segment.end_ms)} · ${source}` : 'nicht erkannt'}
        </span>
        <button type="button" aria-label={`${label} bearbeiten`} onClick={open} className="text-muted hover:text-fg">
          <Pencil className="size-4" />
        </button>
        {segment && (
          <button
            type="button"
            aria-label={`${label} entfernen`}
            onClick={() => remove.mutate(kind)}
            className="text-muted hover:text-fg"
          >
            <Trash2 className="size-4" />
          </button>
        )}
      </div>
      {editing && (
        <form onSubmit={submit} className="mt-2 flex flex-wrap items-end gap-2">
          {[
            { name: 'Anfang', value: start, change: setStart },
            { name: 'Ende', value: end, change: setEnd },
          ].map((field) => (
            <label key={field.name} className="flex flex-col gap-1">
              <span className="text-xs text-subtle">{field.name}</span>
              <input
                aria-label={`${label} ${field.name}`}
                placeholder="m:ss"
                value={field.value}
                onChange={(e) => field.change(e.target.value)}
                className="h-9 w-24 rounded-lg border border-line bg-surface-2 px-2 text-sm focus:border-accent focus:outline-none"
              />
            </label>
          ))}
          <Button type="submit" variant="accent" className="h-9 px-4" loading={set.isPending}>
            Speichern
          </Button>
          <Button type="button" variant="ghost" className="h-9 px-4" onClick={() => onEdit(false)}>
            Abbrechen
          </Button>
        </form>
      )}
      {invalid && <p className="mt-1 text-xs text-red-400">Zeiten als m:ss angeben; das Ende muss nach dem Anfang liegen.</p>}
      <FormError error={set.error ?? remove.error} />
    </li>
  )
}
