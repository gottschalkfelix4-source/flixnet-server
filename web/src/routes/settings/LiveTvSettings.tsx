import clsx from 'clsx'
import { CalendarDays, CheckCircle2, Plus, Radar, RefreshCw, Router, Trash2, TriangleAlert, Wand2 } from 'lucide-react'
import { type FormEvent, useState } from 'react'
import {
  type Channel,
  type DiscoveredTuner,
  type EpgChannel,
  type EpgSource,
  TUNER_KIND_LABELS,
  type Tuner,
  type TunerKind,
  useChannels,
  useCreateEpgSource,
  useCreateTuner,
  useDeleteEpgSource,
  useDeleteTuner,
  useDiscoverTuners,
  useEpgChannels,
  useEpgSources,
  useImportTuner,
  useLiveStreams,
  useRefreshEpgSource,
  useRematchChannels,
  useSetChannelEpg,
  useTuners,
  useUpdateChannel,
  useUpdateEpgSource,
  useUpdateTuner,
} from '@/api/queries/livetv'
import { FormError } from '@/components/layout/AuthLayout'
import { Button } from '@/components/ui/Button'
import { Dialog } from '@/components/ui/Dialog'
import { Checkbox, Section, SelectField } from '@/components/ui/Section'
import { TextField } from '@/components/ui/TextField'
import { relativeTime } from '@/lib/format'

export function LiveTvSettings() {
  return (
    <div className="flex flex-col gap-6">
      <TunersSection />
      <EpgSourcesSection />
      <ChannelsSection />
      <StreamsSection />
    </div>
  )
}

function plural(n: number, one: string, many: string) {
  return `${n.toLocaleString('de')} ${n === 1 ? one : many}`
}

function ErrorLine({ text }: { text: string | null | undefined }) {
  if (!text) return null
  return (
    <p className="mt-1 flex items-start gap-1.5 text-xs text-red-400">
      <TriangleAlert className="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />
      <span className="break-words">{text}</span>
    </p>
  )
}

// ---- Tuners -------------------------------------------------------------------------

function tunerSummary(tuner: Tuner, now: number): string {
  if (tuner.last_import_at == null) return 'Sender werden eingelesen …'
  const tuners =
    tuner.tuner_count > 0 ? `${tuner.tuners_in_use} von ${plural(tuner.tuner_count, 'Tuner', 'Tunern')} belegt` : 'unbegrenzte Streams'
  return [plural(tuner.channel_count, 'Sender', 'Sender'), tuners, `eingelesen ${relativeTime(tuner.last_import_at, now)}`].join(' · ')
}

function TunersSection() {
  const tuners = useTuners()
  const reimport = useImportTuner()
  const [editing, setEditing] = useState<Tuner | 'new' | null>(null)
  const [now] = useState(() => Date.now())

  return (
    <Section
      title="Tuner"
      description="Quellen für Live-Sender: M3U-Playlists, Xtream-Codes-Zugänge und HDHomeRun-Geräte im Netzwerk."
      action={
        <Button variant="accent" className="h-10" onClick={() => setEditing('new')}>
          <Plus className="size-4" aria-hidden="true" />
          Tuner hinzufügen
        </Button>
      }
    >
      {tuners.data?.length === 0 && <p className="text-sm text-muted">Noch keine Tuner eingerichtet.</p>}
      <ul className="divide-y divide-line/60" data-testid="tuner-list">
        {tuners.data?.map((tuner) => (
          <li key={tuner.id} className="flex flex-wrap items-center gap-4 py-3">
            <Router className={clsx('size-5 shrink-0', tuner.enabled ? 'text-accent' : 'text-subtle')} aria-hidden="true" />
            <button type="button" onClick={() => setEditing(tuner)} className="min-w-0 flex-1 text-left">
              <p className="text-sm font-bold">
                {tuner.name}
                <span className="ml-2 rounded-full bg-surface-3 px-2 py-0.5 text-[11px] font-semibold text-muted">
                  {TUNER_KIND_LABELS[tuner.kind]}
                </span>
                {!tuner.enabled && <span className="ml-2 text-xs font-semibold text-subtle">deaktiviert</span>}
              </p>
              <p className="text-xs text-subtle" data-testid="tuner-summary">
                {tunerSummary(tuner, now)}
              </p>
              <ErrorLine text={tuner.last_error} />
            </button>
            <Button
              variant="ghost"
              className="h-9 px-4"
              onClick={() => reimport.mutate(tuner.id)}
              loading={reimport.isPending && reimport.variables === tuner.id}
            >
              <RefreshCw className="size-4" aria-hidden="true" />
              Neu einlesen
            </Button>
          </li>
        ))}
      </ul>
      {editing && (
        <TunerDialog
          key={editing === 'new' ? 'new' : editing.id}
          tuner={editing === 'new' ? null : editing}
          onClose={() => setEditing(null)}
        />
      )}
    </Section>
  )
}

const URL_LABELS: Record<TunerKind, { label: string; placeholder: string }> = {
  m3u: { label: 'Playlist (URL oder Datei)', placeholder: 'http://anbieter.example/playlist.m3u' },
  xtream: { label: 'Server-Adresse', placeholder: 'http://anbieter.example:8080' },
  hdhomerun: { label: 'IP-Adresse des Geräts', placeholder: '192.168.1.50' },
}

function TunerDialog({ tuner, onClose }: { tuner: Tuner | null; onClose: () => void }) {
  const create = useCreateTuner()
  const update = useUpdateTuner()
  const remove = useDeleteTuner()
  const discover = useDiscoverTuners()
  const [kind, setKind] = useState<TunerKind>(tuner?.kind ?? 'm3u')
  const [name, setName] = useState(tuner?.name ?? '')
  const [url, setUrl] = useState(tuner?.url ?? '')
  const [username, setUsername] = useState(tuner?.username ?? '')
  const [password, setPassword] = useState('')
  const [userAgent, setUserAgent] = useState(tuner?.user_agent ?? '')
  const [count, setCount] = useState(tuner?.tuner_count_manual ? String(tuner.tuner_count) : '')
  const [enabled, setEnabled] = useState(tuner?.enabled ?? true)
  const [confirmDelete, setConfirmDelete] = useState(false)
  const error = create.error ?? update.error ?? remove.error ?? discover.error

  function submit(event: FormEvent) {
    event.preventDefault()
    const tunerCount = count.trim() === '' ? null : Number(count)
    if (tuner) {
      update.mutate(
        {
          id: tuner.id,
          name: name.trim(),
          url: url.trim(),
          username: kind === 'xtream' ? username.trim() || null : undefined,
          password: kind === 'xtream' && password ? password : undefined,
          user_agent: userAgent.trim() || null,
          tuner_count: tunerCount,
          enabled,
        },
        { onSuccess: onClose },
      )
    } else {
      create.mutate(
        {
          kind,
          name: name.trim(),
          url: url.trim(),
          username: kind === 'xtream' ? username.trim() : null,
          password: kind === 'xtream' ? password : null,
          user_agent: userAgent.trim() || null,
          tuner_count: tunerCount,
        },
        { onSuccess: onClose },
      )
    }
  }

  function pick(device: DiscoveredTuner) {
    setUrl(device.url)
    if (!name.trim()) setName(device.name ?? `HDHomeRun ${device.device_id}`)
  }

  return (
    <Dialog
      open
      onOpenChange={(open) => !open && onClose()}
      title={tuner ? `${tuner.name} bearbeiten` : 'Tuner hinzufügen'}
      description={tuner ? undefined : 'Die Senderliste wird nach dem Speichern im Hintergrund eingelesen.'}
    >
      <form onSubmit={submit} className="flex flex-col gap-4">
        {!tuner && (
          <SelectField
            label="Art"
            value={kind}
            onChange={(v) => setKind(v as TunerKind)}
            options={(Object.keys(TUNER_KIND_LABELS) as TunerKind[]).map((k) => ({ value: k, label: TUNER_KIND_LABELS[k] }))}
          />
        )}
        <TextField label="Name" value={name} maxLength={64} onChange={(e) => setName(e.target.value)} />
        <TextField
          label={URL_LABELS[kind].label}
          placeholder={URL_LABELS[kind].placeholder}
          value={url}
          onChange={(e) => setUrl(e.target.value)}
        />
        {kind === 'hdhomerun' && (
          <div className="flex flex-col gap-2">
            <Button type="button" variant="ghost" className="h-9 self-start px-4" loading={discover.isPending} onClick={() => discover.mutate(undefined)}>
              <Radar className="size-4" aria-hidden="true" />
              Im Netzwerk suchen
            </Button>
            {discover.data?.length === 0 && (
              <p className="text-xs text-subtle">
                Kein Gerät gefunden. In Docker braucht die Suche das Host-Netzwerk – sonst die IP-Adresse eintragen.
              </p>
            )}
            {discover.data?.map((device) => (
              <button
                key={device.device_id}
                type="button"
                onClick={() => pick(device)}
                className="flex items-center justify-between gap-3 rounded-lg bg-surface-2 px-3 py-2 text-left text-sm hover:bg-surface-3"
              >
                <span>
                  <span className="font-semibold">{device.name ?? 'HDHomeRun'}</span>
                  <span className="ml-2 text-xs text-subtle">
                    {[device.model, device.device_id, device.tuner_count ? plural(device.tuner_count, 'Tuner', 'Tuner') : null]
                      .filter(Boolean)
                      .join(' · ')}
                  </span>
                </span>
                <span className="text-xs text-muted">{device.configured ? 'bereits eingerichtet' : device.url}</span>
              </button>
            ))}
          </div>
        )}
        {kind === 'xtream' && (
          <div className="grid gap-4 md:grid-cols-2">
            <TextField label="Benutzername" autoComplete="off" value={username} onChange={(e) => setUsername(e.target.value)} />
            <TextField
              label={tuner?.has_password ? 'Neues Passwort (gespeichert)' : 'Passwort'}
              type="password"
              autoComplete="new-password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
            />
          </div>
        )}
        <div className="grid gap-4 md:grid-cols-2">
          <TextField
            label="Gleichzeitige Streams"
            hint={kind === 'm3u' ? 'Leer = unbegrenzt' : 'Leer = vom Gerät bzw. Zugang'}
            inputMode="numeric"
            value={count}
            onChange={(e) => setCount(e.target.value.replace(/\D/g, ''))}
          />
          {kind !== 'hdhomerun' && (
            <TextField label="User-Agent (optional)" value={userAgent} onChange={(e) => setUserAgent(e.target.value)} />
          )}
        </div>
        {tuner && <Checkbox label="Aktiv" hint="Deaktivierte Tuner blenden ihre Sender aus." checked={enabled} onChange={setEnabled} />}
        <FormError error={error} />
        <div className="flex flex-wrap items-center justify-between gap-3 pt-2">
          {tuner ? (
            confirmDelete ? (
              <Button
                type="button"
                className="bg-red-500 text-white hover:bg-red-400"
                loading={remove.isPending}
                onClick={() => remove.mutate(tuner.id, { onSuccess: onClose })}
              >
                Tuner entfernen
              </Button>
            ) : (
              <Button type="button" variant="ghost" onClick={() => setConfirmDelete(true)}>
                <Trash2 className="size-4" aria-hidden="true" />
                Entfernen
              </Button>
            )
          ) : (
            <span />
          )}
          <Button type="submit" variant="accent" loading={create.isPending || update.isPending} disabled={!name.trim() || !url.trim()}>
            Speichern
          </Button>
        </div>
      </form>
    </Dialog>
  )
}

// ---- Guide sources ------------------------------------------------------------------

function EpgSourcesSection() {
  const sources = useEpgSources()
  const refresh = useRefreshEpgSource()
  const [editing, setEditing] = useState<EpgSource | 'new' | null>(null)
  const [now] = useState(() => Date.now())

  return (
    <Section
      title="Programmquellen (XMLTV)"
      description="Das TV-Programm kommt aus XMLTV-Dateien (URL oder Datei, auch .gz). Flixnet lädt sie zweimal täglich neu."
      action={
        <Button variant="accent" className="h-10" onClick={() => setEditing('new')}>
          <Plus className="size-4" aria-hidden="true" />
          Programmquelle hinzufügen
        </Button>
      }
    >
      {sources.data?.length === 0 && <p className="text-sm text-muted">Noch keine Programmquelle eingerichtet.</p>}
      <ul className="divide-y divide-line/60" data-testid="epg-source-list">
        {sources.data?.map((source) => (
          <li key={source.id} className="flex flex-wrap items-center gap-4 py-3">
            <CalendarDays className={clsx('size-5 shrink-0', source.enabled ? 'text-accent' : 'text-subtle')} aria-hidden="true" />
            <button type="button" onClick={() => setEditing(source)} className="min-w-0 flex-1 text-left">
              <p className="text-sm font-bold">
                {source.name}
                {!source.enabled && <span className="ml-2 text-xs font-semibold text-subtle">deaktiviert</span>}
              </p>
              <p className="text-xs text-subtle" data-testid="epg-source-summary">
                {source.last_refresh_at == null
                  ? 'Programm wird geladen …'
                  : [
                      plural(source.channel_count, 'Sender', 'Sender'),
                      plural(source.program_count, 'Sendung', 'Sendungen'),
                      `geladen ${relativeTime(source.last_refresh_at, now)}`,
                    ].join(' · ')}
              </p>
              <ErrorLine text={source.last_error} />
            </button>
            <Button
              variant="ghost"
              className="h-9 px-4"
              onClick={() => refresh.mutate(source.id)}
              loading={refresh.isPending && refresh.variables === source.id}
            >
              <RefreshCw className="size-4" aria-hidden="true" />
              Neu laden
            </Button>
          </li>
        ))}
      </ul>
      {editing && (
        <EpgSourceDialog
          key={editing === 'new' ? 'new' : editing.id}
          source={editing === 'new' ? null : editing}
          onClose={() => setEditing(null)}
        />
      )}
    </Section>
  )
}

function EpgSourceDialog({ source, onClose }: { source: EpgSource | null; onClose: () => void }) {
  const create = useCreateEpgSource()
  const update = useUpdateEpgSource()
  const remove = useDeleteEpgSource()
  const [name, setName] = useState(source?.name ?? '')
  const [url, setUrl] = useState(source?.url ?? '')
  const [enabled, setEnabled] = useState(source?.enabled ?? true)
  const [confirmDelete, setConfirmDelete] = useState(false)
  const error = create.error ?? update.error ?? remove.error

  function submit(event: FormEvent) {
    event.preventDefault()
    if (source) update.mutate({ id: source.id, name: name.trim(), url: url.trim(), enabled }, { onSuccess: onClose })
    else create.mutate({ name: name.trim(), url: url.trim() }, { onSuccess: onClose })
  }

  return (
    <Dialog
      open
      onOpenChange={(open) => !open && onClose()}
      title={source ? `${source.name} bearbeiten` : 'Programmquelle hinzufügen'}
    >
      <form onSubmit={submit} className="flex flex-col gap-4">
        <TextField label="Name" value={name} maxLength={64} onChange={(e) => setName(e.target.value)} />
        <TextField
          label="XMLTV-Adresse oder Datei"
          placeholder="http://epg.example/guide.xml.gz"
          value={url}
          onChange={(e) => setUrl(e.target.value)}
        />
        {source && <Checkbox label="Aktiv" checked={enabled} onChange={setEnabled} />}
        <FormError error={error} />
        <div className="flex flex-wrap items-center justify-between gap-3 pt-2">
          {source ? (
            confirmDelete ? (
              <Button
                type="button"
                className="bg-red-500 text-white hover:bg-red-400"
                loading={remove.isPending}
                onClick={() => remove.mutate(source.id, { onSuccess: onClose })}
              >
                Quelle entfernen
              </Button>
            ) : (
              <Button type="button" variant="ghost" onClick={() => setConfirmDelete(true)}>
                <Trash2 className="size-4" aria-hidden="true" />
                Entfernen
              </Button>
            )
          ) : (
            <span />
          )}
          <Button type="submit" variant="accent" loading={create.isPending || update.isPending} disabled={!name.trim() || !url.trim()}>
            Speichern
          </Button>
        </div>
      </form>
    </Dialog>
  )
}

// ---- Channels and guide matching ----------------------------------------------------

function epgName(channels: EpgChannel[] | undefined, sourceId: number, channelId: string): string {
  const found = channels?.find((c) => c.epg_source_id === sourceId && c.channel_id === channelId)
  return found?.names[0] ?? channelId
}

function ChannelsSection() {
  const channels = useChannels(true, 4000)
  const sources = useEpgSources()
  const epgChannels = useEpgChannels((sources.data ?? []).map((s) => `${s.id}:${s.last_refresh_at}:${s.enabled}`).join(','))
  const update = useUpdateChannel()
  const rematch = useRematchChannels()
  const [filter, setFilter] = useState('')
  const [assigning, setAssigning] = useState<Channel | null>(null)
  const list = channels.data ?? []
  const needle = filter.trim().toLowerCase()
  const shown = needle ? list.filter((c) => `${c.number ?? ''} ${c.name}`.toLowerCase().includes(needle)) : list
  const matched = list.filter((c) => c.epg).length

  return (
    <Section
      title="Sender & Programmzuordnung"
      description={
        list.length > 0
          ? `${matched} von ${plural(list.length, 'Sender', 'Sendern')} haben ein Programm. Zuordnungen von Hand bleiben beim Neu-Einlesen erhalten.`
          : 'Sender erscheinen hier, sobald ein Tuner eingelesen ist.'
      }
      action={
        list.length > 0 && (
          <Button variant="ghost" className="h-10" loading={rematch.isPending} onClick={() => rematch.mutate()}>
            <Wand2 className="size-4" aria-hidden="true" />
            Automatisch zuordnen
          </Button>
        )
      }
    >
      {list.length > 10 && (
        <input
          type="search"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          placeholder="Sender filtern"
          aria-label="Sender filtern"
          className="mb-3 h-10 w-full max-w-xs rounded-lg border border-line bg-surface-2 px-3 text-sm focus:border-accent focus:outline-none"
        />
      )}
      <ul className="max-h-[32rem] divide-y divide-line/60 overflow-y-auto" data-testid="channel-mapping">
        {shown.map((channel) => (
          <li key={channel.id} className="flex flex-wrap items-center gap-3 py-2.5" data-testid="channel-mapping-row">
            <input
              type="checkbox"
              checked={channel.enabled}
              onChange={(e) => update.mutate({ id: channel.id, enabled: e.target.checked })}
              aria-label={`${channel.name} anzeigen`}
              className="size-4 accent-[var(--color-accent)]"
            />
            <span className="w-12 shrink-0 text-xs font-bold text-subtle">{channel.number}</span>
            <span className={clsx('min-w-0 flex-1 truncate text-sm font-semibold', !channel.enabled && 'text-subtle line-through')}>
              {channel.name}
            </span>
            <button
              type="button"
              onClick={() => setAssigning(channel)}
              className="flex min-w-0 max-w-72 items-center gap-2 rounded-lg px-2.5 py-1 text-left text-xs hover:bg-surface-2"
              data-testid="channel-epg"
            >
              {channel.epg ? (
                <>
                  <CheckCircle2
                    className={clsx('size-3.5 shrink-0', channel.epg.kind === 'manual' ? 'text-accent' : 'text-emerald-400')}
                    aria-hidden="true"
                  />
                  <span className="truncate">{epgName(epgChannels.data, channel.epg.epg_source_id, channel.epg.epg_channel_id)}</span>
                  <span className="shrink-0 text-subtle">
                    {channel.epg.kind === 'manual' ? 'manuell' : `${Math.round((channel.epg.score ?? 0) * 100)} %`}
                  </span>
                </>
              ) : (
                <span className="text-subtle">{channel.epg_disabled ? 'Kein Programm (manuell)' : 'Nicht zugeordnet'}</span>
              )}
            </button>
          </li>
        ))}
      </ul>
      {assigning && (
        <AssignDialog channel={assigning} epgChannels={epgChannels.data ?? []} onClose={() => setAssigning(null)} />
      )}
    </Section>
  )
}

function AssignDialog({ channel, epgChannels, onClose }: { channel: Channel; epgChannels: EpgChannel[]; onClose: () => void }) {
  const assign = useSetChannelEpg()
  const [search, setSearch] = useState('')
  const needle = search.trim().toLowerCase()
  const options = (needle
    ? epgChannels.filter((c) => [c.channel_id, ...c.names].some((n) => n.toLowerCase().includes(needle)))
    : epgChannels
  ).slice(0, 100)
  const done = { onSuccess: onClose }

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()} title={`Programm für ${channel.name}`}>
      <div className="flex flex-col gap-3">
        <div className="flex flex-wrap gap-2">
          <Button type="button" variant="ghost" className="h-9 px-4" onClick={() => assign.mutate({ id: channel.id, mode: 'auto' }, done)}>
            <Wand2 className="size-4" aria-hidden="true" />
            Automatisch
          </Button>
          <Button type="button" variant="ghost" className="h-9 px-4" onClick={() => assign.mutate({ id: channel.id, mode: 'none' }, done)}>
            Kein Programm
          </Button>
        </div>
        <input
          type="search"
          autoFocus
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="Programm-Sender suchen"
          aria-label="Programm-Sender suchen"
          className="h-10 rounded-lg border border-line bg-surface-2 px-3 text-sm focus:border-accent focus:outline-none"
        />
        <ul className="max-h-72 overflow-y-auto" data-testid="epg-channel-options">
          {options.map((option) => {
            const current =
              channel.epg?.epg_source_id === option.epg_source_id && channel.epg?.epg_channel_id === option.channel_id
            return (
              <li key={`${option.epg_source_id}:${option.channel_id}`}>
                <button
                  type="button"
                  onClick={() =>
                    assign.mutate(
                      { id: channel.id, mode: 'manual', epg_source_id: option.epg_source_id, epg_channel_id: option.channel_id },
                      done,
                    )
                  }
                  className={clsx(
                    'flex w-full items-center justify-between gap-3 rounded-lg px-3 py-2 text-left text-sm hover:bg-surface-2',
                    current && 'text-accent',
                  )}
                >
                  <span className="truncate font-semibold">{option.names[0] ?? option.channel_id}</span>
                  <span className="shrink-0 font-mono text-xs text-subtle">{option.channel_id}</span>
                </button>
              </li>
            )
          })}
          {options.length === 0 && <li className="px-3 py-2 text-sm text-muted">Kein Programm-Sender gefunden.</li>}
        </ul>
        <FormError error={assign.error} />
      </div>
    </Dialog>
  )
}

// ---- Running streams ----------------------------------------------------------------

function StreamsSection() {
  const streams = useLiveStreams()
  const [now] = useState(() => Date.now())
  if (!streams.data?.length) return null
  return (
    <Section title="Laufende Streams" description="Zuschauer eines Senders teilen sich einen Stream und belegen nur einen Tuner.">
      <ul className="divide-y divide-line/60" data-testid="live-streams">
        {streams.data.map((stream) => (
          <li key={stream.id} className="flex flex-wrap items-center gap-x-6 gap-y-1 py-2.5 text-sm">
            <span className="font-bold">{stream.channel_name ?? `Sender ${stream.channel_id}`}</span>
            <span className="text-muted">{plural(stream.viewers.length, 'Zuschauer', 'Zuschauer')}</span>
            <span className="text-muted">
              {stream.video_copied && stream.audio_copied ? 'Kopie' : 'Transcode'} · seit {relativeTime(stream.started_at, now)}
            </span>
          </li>
        ))}
      </ul>
    </Section>
  )
}
