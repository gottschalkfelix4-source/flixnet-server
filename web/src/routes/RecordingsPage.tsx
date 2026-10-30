import { Link } from '@tanstack/react-router'
import clsx from 'clsx'
import { ArrowLeft, ArrowUp, CircleDot, Film, Play, Square, Trash2, TriangleAlert, X } from 'lucide-react'
import { useState } from 'react'
import {
  RECORDING_STATUS_LABELS,
  type Recording,
  type RecordingRule,
  formatBytes,
  formatDay,
  formatSlot,
  useDeleteRecording,
  useDeleteRecordingRule,
  useRecordingRules,
  useRecordings,
  useStopRecording,
  useUpdateRecordingRule,
} from '@/api/queries/livetv'
import { Button } from '@/components/ui/Button'
import { EmptyState } from '@/components/ui/EmptyState'
import { PageHeader } from '@/components/ui/PageHeader'

type Tab = 'scheduled' | 'recording' | 'done' | 'conflicts' | 'rules'

const TABS: { id: Tab; label: string }[] = [
  { id: 'scheduled', label: 'Geplant' },
  { id: 'recording', label: 'Läuft' },
  { id: 'done', label: 'Fertig' },
  { id: 'conflicts', label: 'Konflikte' },
  { id: 'rules', label: 'Serien & Regeln' },
]

function inTab(tab: Tab, r: Recording): boolean {
  switch (tab) {
    case 'scheduled':
      return r.status === 'scheduled' && !r.conflict
    case 'recording':
      return r.status === 'recording'
    case 'done':
      return r.status === 'completed' || r.status === 'failed'
    case 'conflicts':
      return r.status === 'scheduled' && r.conflict
    case 'rules':
      return false
  }
}

/** Recordings: scheduled, running, done, conflicts, and the rules behind them. */
export function RecordingsPage() {
  const recordings = useRecordings()
  const rules = useRecordingRules()
  const [tab, setTab] = useState<Tab>('scheduled')
  const all = recordings.data ?? []
  const count = (t: Tab) => (t === 'rules' ? (rules.data?.length ?? 0) : all.filter((r) => inTab(t, r)).length)
  const shown = all.filter((r) => inTab(tab, r))
  if (tab === 'done') shown.reverse()

  return (
    <div className="pb-10">
      <PageHeader title="Aufnahmen">
        <Link to="/live" className="inline-flex items-center gap-1.5 text-sm font-semibold text-muted hover:text-fg">
          <ArrowLeft className="size-4" aria-hidden="true" />
          Zum Programm
        </Link>
      </PageHeader>
      <div className="flex flex-col gap-6 px-6 md:px-10">
        <div className="flex flex-wrap gap-2" role="tablist" aria-label="Aufnahmen">
          {TABS.map((t) => (
            <button
              key={t.id}
              type="button"
              role="tab"
              aria-selected={tab === t.id}
              onClick={() => setTab(t.id)}
              className={clsx(
                'inline-flex h-9 items-center gap-2 rounded-full px-4 text-sm font-semibold transition-colors',
                tab === t.id ? 'bg-fg text-canvas' : 'bg-surface-2 text-muted hover:text-fg',
                t.id === 'conflicts' && count('conflicts') > 0 && tab !== t.id && 'text-amber-400',
              )}
            >
              {t.label}
              <span className="text-xs opacity-70">{count(t.id)}</span>
            </button>
          ))}
        </div>

        {tab === 'rules' ? (
          <RuleList rules={rules.data ?? []} />
        ) : recordings.isPending ? (
          <p className="text-sm text-muted">Wird geladen …</p>
        ) : shown.length === 0 ? (
          <EmptyState icon={CircleDot} title={tab === 'conflicts' ? 'Keine Konflikte' : 'Keine Aufnahmen'}>
            Im Programm eine Sendung öffnen und „Aufnehmen“ oder „Serie aufnehmen“ wählen.
          </EmptyState>
        ) : (
          <ul className="flex flex-col divide-y divide-line/60" data-testid="recording-list">
            {shown.map((r) => (
              <RecordingRow key={r.id} recording={r} />
            ))}
          </ul>
        )}
      </div>
    </div>
  )
}

function RecordingRow({ recording: r }: { recording: Recording }) {
  const remove = useDeleteRecording()
  const stop = useStopRecording()
  const raise = useUpdateRecordingRule()
  const episode =
    r.season != null && r.episode != null
      ? `S${String(r.season).padStart(2, '0')}E${String(r.episode).padStart(2, '0')}`
      : null
  return (
    <li className="flex flex-wrap items-center gap-x-4 gap-y-2 py-3" data-testid="recording-row">
      <div className="min-w-0 flex-1">
        <p className="truncate font-bold">
          {r.title}
          {r.subtitle && <span className="font-normal text-muted"> · {r.subtitle}</span>}
        </p>
        <p className="text-sm text-muted">
          {[r.channel_name, formatDay(r.start_ms), formatSlot(r), episode, r.rule_kind === 'series' ? 'Serie' : null]
            .filter(Boolean)
            .join(' · ')}
        </p>
        {r.error && r.status !== 'completed' && <p className="text-sm text-amber-400">{r.error}</p>}
      </div>
      <span
        className={clsx(
          'inline-flex items-center gap-1.5 rounded-full px-3 py-1 text-xs font-bold',
          r.status === 'recording' && 'bg-red-600 text-white',
          r.status === 'scheduled' && (r.conflict ? 'bg-amber-500/15 text-amber-400' : 'bg-surface-3 text-muted'),
          r.status === 'completed' && 'bg-accent/15 text-accent',
          (r.status === 'failed' || r.status === 'cancelled') && 'bg-surface-3 text-subtle',
        )}
        data-testid="recording-status"
      >
        {r.status === 'recording' && <CircleDot className="size-3 animate-pulse" aria-hidden="true" />}
        {r.conflict && r.status === 'scheduled' && <TriangleAlert className="size-3" aria-hidden="true" />}
        {r.conflict && r.status === 'scheduled' ? 'Konflikt' : RECORDING_STATUS_LABELS[r.status]}
        {(r.status === 'recording' || r.status === 'completed') && r.bytes > 0 && ` · ${formatBytes(r.bytes)}`}
        {r.parts > 1 && ` · ${r.parts} Teile`}
      </span>
      <div className="flex items-center gap-1">
        {r.conflict && r.status === 'scheduled' && r.rule_id != null && (
          <Button
            variant="ghost"
            className="h-9 px-3"
            loading={raise.isPending}
            onClick={() => raise.mutate({ id: r.rule_id!, priority: r.priority + 1 })}
          >
            <ArrowUp className="size-4" aria-hidden="true" />
            Vorziehen
          </Button>
        )}
        {r.status === 'completed' && r.item_id != null && (
          <Link
            to="/item/$id"
            params={{ id: String(r.item_id) }}
            className="inline-flex h-9 items-center gap-1.5 rounded-full bg-accent px-4 text-sm font-bold text-accent-fg hover:bg-accent-hover"
            data-testid="recording-item"
          >
            <Film className="size-4" aria-hidden="true" />
            In der Bibliothek
          </Link>
        )}
        {r.status === 'recording' && r.channel_id != null && (
          <Link
            to="/live/$channelId"
            params={{ channelId: String(r.channel_id) }}
            aria-label="Live ansehen"
            className="flex size-9 items-center justify-center rounded-full hover:bg-surface-2"
          >
            <Play className="size-4" aria-hidden="true" />
          </Link>
        )}
        {r.status === 'recording' ? (
          <Button variant="ghost" className="h-9 px-3" loading={stop.isPending} onClick={() => stop.mutate(r.id)}>
            <Square className="size-4" aria-hidden="true" />
            Stoppen
          </Button>
        ) : (
          <button
            type="button"
            aria-label={r.status === 'scheduled' ? 'Aufnahme abbrechen' : 'Aufnahme löschen'}
            onClick={() => remove.mutate(r.id)}
            disabled={remove.isPending}
            className="flex size-9 items-center justify-center rounded-full text-muted hover:bg-surface-2 hover:text-fg"
          >
            {r.status === 'scheduled' ? <X className="size-4" /> : <Trash2 className="size-4" />}
          </button>
        )}
      </div>
    </li>
  )
}

function windowLabel(rule: RecordingRule): string | null {
  if (rule.window_start_min == null || rule.window_end_min == null) return null
  const hm = (m: number) => `${String(Math.floor(m / 60) % 24).padStart(2, '0')}:${String(m % 60).padStart(2, '0')}`
  return `${hm(rule.window_start_min)}–${hm(rule.window_end_min)} Uhr`
}

function RuleList({ rules }: { rules: RecordingRule[] }) {
  const update = useUpdateRecordingRule()
  const remove = useDeleteRecordingRule()
  const series = rules.filter((r) => r.kind === 'series')
  if (series.length === 0) {
    return (
      <EmptyState icon={CircleDot} title="Keine Serien">
        Öffne eine Sendung im Programm und wähle „Serie aufnehmen“.
      </EmptyState>
    )
  }
  return (
    <ul className="flex flex-col divide-y divide-line/60" data-testid="rule-list">
      {series.map((rule) => (
        <li key={rule.id} className="flex flex-wrap items-center gap-x-4 gap-y-2 py-3">
          <div className="min-w-0 flex-1">
            <p className="truncate font-bold">{rule.title}</p>
            <p className="text-sm text-muted">
              {[
                rule.channel_name ?? 'Alle Sender',
                windowLabel(rule),
                rule.new_only ? 'Nur neue Folgen' : 'Alle Folgen',
                rule.keep_last ? `Behält ${rule.keep_last}` : null,
                `${rule.scheduled} geplant`,
                rule.conflicts > 0 ? `${rule.conflicts} Konflikte` : null,
                rule.next_start_ms ? `nächste ${formatDay(rule.next_start_ms)}` : null,
              ]
                .filter(Boolean)
                .join(' · ')}
            </p>
          </div>
          {rule.can_edit && (
            <div className="flex items-center gap-2">
              <span className="text-sm text-muted">Priorität {rule.priority}</span>
              <Button
                variant="ghost"
                className="h-9 px-3"
                aria-label="Priorität erhöhen"
                onClick={() => update.mutate({ id: rule.id, priority: Math.min(100, rule.priority + 1) })}
              >
                <ArrowUp className="size-4" aria-hidden="true" />
              </Button>
              <Button
                variant="ghost"
                className="h-9 px-3"
                onClick={() => update.mutate({ id: rule.id, new_only: !rule.new_only })}
              >
                {rule.new_only ? 'Alle Folgen' : 'Nur neue'}
              </Button>
              <button
                type="button"
                aria-label="Serie nicht mehr aufnehmen"
                onClick={() => remove.mutate(rule.id)}
                className="flex size-9 items-center justify-center rounded-full text-muted hover:bg-surface-2 hover:text-fg"
              >
                <Trash2 className="size-4" />
              </button>
            </div>
          )}
        </li>
      ))}
    </ul>
  )
}
