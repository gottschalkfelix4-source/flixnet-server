import clsx from 'clsx'
import { Bell, Mail, MessageSquare, Pencil, Plus, Send, Trash2, Webhook } from 'lucide-react'
import { type FormEvent, useState } from 'react'
import {
  type NotificationTarget,
  type NotificationTargetInput,
  useDeleteNotificationTarget,
  useNotificationEvents,
  useNotificationTargets,
  useSaveNotificationTarget,
  useSmtpSettings,
  useTestNotificationTarget,
  useUpdateSmtpSettings,
} from '@/api/queries/ops'
import type { Schemas } from '@/api/client'
import { FormError } from '@/components/layout/AuthLayout'
import { Button } from '@/components/ui/Button'
import { Dialog } from '@/components/ui/Dialog'
import { Checkbox, Section, SelectField } from '@/components/ui/Section'
import { TextField } from '@/components/ui/TextField'

type Kind = Schemas['TargetKind']

const KINDS: { value: Kind; label: string; icon: typeof Bell }[] = [
  { value: 'webhook', label: 'Webhook', icon: Webhook },
  { value: 'ntfy', label: 'ntfy', icon: Bell },
  { value: 'discord', label: 'Discord', icon: MessageSquare },
  { value: 'email', label: 'E-Mail', icon: Mail },
]

const DEFAULT_EVENTS = ['library.items_added', 'library.scan_failed', 'transcode.failed', 'user.new_device']

interface Draft {
  name: string
  kind: Kind
  enabled: boolean
  events: string[]
  url: string
  secret: string
  server_url: string
  topic: string
  token: string
  priority: string
  webhook_url: string
  recipients: string
}

function draftOf(target?: NotificationTarget): Draft {
  return {
    name: target?.name ?? '',
    kind: target?.kind ?? 'webhook',
    enabled: target?.enabled ?? true,
    events: target?.events ?? DEFAULT_EVENTS,
    url: target?.url ?? '',
    secret: '',
    server_url: target?.server_url ?? 'https://ntfy.sh',
    topic: target?.topic ?? '',
    token: '',
    priority: String(target?.priority ?? 3),
    webhook_url: '',
    recipients: target?.recipients.join(', ') ?? '',
  }
}

function TargetDialog({ target, onClose }: { target: NotificationTarget | null | undefined; onClose: () => void }) {
  const events = useNotificationEvents()
  const save = useSaveNotificationTarget()
  const [draft, setDraft] = useState<Draft>(() => draftOf(target ?? undefined))
  const editing = target != null
  const set = <K extends keyof Draft>(key: K, value: Draft[K]) => setDraft((d) => ({ ...d, [key]: value }))

  function submit(event: FormEvent) {
    event.preventDefault()
    const body: NotificationTargetInput = {
      name: draft.name.trim(),
      kind: draft.kind,
      enabled: draft.enabled,
      events: draft.events,
    }
    if (draft.kind === 'webhook') {
      body.url = draft.url.trim()
      if (draft.secret) body.secret = draft.secret
    } else if (draft.kind === 'ntfy') {
      body.server_url = draft.server_url.trim()
      body.topic = draft.topic.trim()
      body.priority = Number(draft.priority)
      if (draft.token) body.token = draft.token
    } else if (draft.kind === 'discord') {
      if (draft.webhook_url) body.webhook_url = draft.webhook_url.trim()
    } else {
      body.recipients = draft.recipients
        .split(/[,;\s]+/)
        .map((r) => r.trim())
        .filter(Boolean)
    }
    save.mutate({ id: target?.id, body }, { onSuccess: onClose })
  }

  return (
    <Dialog
      open
      onOpenChange={(open) => !open && onClose()}
      title={editing ? 'Ziel bearbeiten' : 'Neues Benachrichtigungsziel'}
    >
      <form onSubmit={submit} className="flex flex-col gap-4">
        <TextField label="Name" required value={draft.name} onChange={(e) => set('name', e.target.value)} autoFocus />
        {!editing && (
          <SelectField
            label="Art"
            value={draft.kind}
            onChange={(v) => set('kind', v as Kind)}
            options={KINDS.map((k) => ({ value: k.value, label: k.label }))}
          />
        )}
        {draft.kind === 'webhook' && (
          <>
            <TextField
              label="Adresse"
              type="url"
              required
              placeholder="https://…"
              value={draft.url}
              onChange={(e) => set('url', e.target.value)}
            />
            <TextField
              label={target?.has_secret ? 'Neues Signatur-Geheimnis (gespeichert)' : 'Signatur-Geheimnis (optional)'}
              type="password"
              autoComplete="off"
              hint="Signiert jede Anfrage per HMAC-SHA256 im Header X-Flixnet-Signature."
              value={draft.secret}
              onChange={(e) => set('secret', e.target.value)}
            />
          </>
        )}
        {draft.kind === 'ntfy' && (
          <>
            <TextField label="Server" type="url" value={draft.server_url} onChange={(e) => set('server_url', e.target.value)} />
            <TextField label="Thema" required value={draft.topic} onChange={(e) => set('topic', e.target.value)} />
            <TextField
              label={target?.has_token ? 'Neues Zugriffstoken (gespeichert)' : 'Zugriffstoken (optional)'}
              type="password"
              autoComplete="off"
              value={draft.token}
              onChange={(e) => set('token', e.target.value)}
            />
            <SelectField
              label="Priorität"
              value={draft.priority}
              onChange={(v) => set('priority', v)}
              options={['1', '2', '3', '4', '5'].map((p) => ({ value: p, label: p === '3' ? '3 (normal)' : p }))}
            />
          </>
        )}
        {draft.kind === 'discord' && (
          <TextField
            label={target?.webhook_url ? `Neue Webhook-Adresse (${target.webhook_url})` : 'Webhook-Adresse'}
            type="url"
            required={!editing}
            autoComplete="off"
            placeholder="https://discord.com/api/webhooks/…"
            value={draft.webhook_url}
            onChange={(e) => set('webhook_url', e.target.value)}
          />
        )}
        {draft.kind === 'email' && (
          <TextField
            label="Empfänger"
            required
            hint="Mehrere Adressen mit Komma trennen. Der SMTP-Server wird unten eingerichtet."
            value={draft.recipients}
            onChange={(e) => set('recipients', e.target.value)}
          />
        )}
        <fieldset className="flex flex-col gap-2">
          <legend className="mb-2 text-sm font-semibold text-muted">Ereignisse</legend>
          {events.data?.map((e) => (
            <Checkbox
              key={e.kind}
              label={e.label}
              checked={draft.events.includes(e.kind)}
              onChange={(checked) =>
                set('events', checked ? [...draft.events, e.kind] : draft.events.filter((k) => k !== e.kind))
              }
            />
          ))}
        </fieldset>
        <Checkbox label="Aktiv" checked={draft.enabled} onChange={(v) => set('enabled', v)} />
        <FormError error={save.error} />
        <Button type="submit" variant="accent" loading={save.isPending}>
          Speichern
        </Button>
      </form>
    </Dialog>
  )
}

function TargetRow({ target, onEdit }: { target: NotificationTarget; onEdit: () => void }) {
  const test = useTestNotificationTarget()
  const remove = useDeleteNotificationTarget()
  const kind = KINDS.find((k) => k.value === target.kind) ?? KINDS[0]!
  const Icon = kind.icon
  const where =
    target.kind === 'webhook'
      ? target.url
      : target.kind === 'ntfy'
        ? `${target.server_url}/${target.topic}`
        : target.kind === 'discord'
          ? target.webhook_url
          : target.recipients.join(', ')

  return (
    <li className="flex flex-wrap items-center gap-3 py-3" data-testid="notification-target">
      <Icon className="size-5 text-muted" aria-hidden="true" />
      <div className="min-w-0 flex-1">
        <p className="flex items-center gap-2 text-sm font-semibold">
          {target.name}
          <span className="rounded-full bg-surface-3 px-2 py-0.5 text-xs text-muted">{kind.label}</span>
          {!target.enabled && <span className="text-xs text-subtle">deaktiviert</span>}
        </p>
        <p className="truncate text-xs text-subtle">
          {where} · {target.events.length} Ereignisse
        </p>
        {test.data && (
          <p
            role="status"
            className={clsx('mt-1 text-xs', test.data.ok ? 'text-emerald-400' : 'text-red-400')}
            data-testid="test-result"
          >
            {test.data.ok ? 'Testnachricht wurde zugestellt.' : `Fehlgeschlagen: ${test.data.error}`}
          </p>
        )}
      </div>
      <Button variant="ghost" className="h-9 px-4" loading={test.isPending} onClick={() => test.mutate(target.id)}>
        <Send className="size-4" aria-hidden="true" />
        Test senden
      </Button>
      <Button variant="ghost" className="h-9 px-3" aria-label="Bearbeiten" onClick={onEdit}>
        <Pencil className="size-4" />
      </Button>
      <Button variant="ghost" className="h-9 px-3" aria-label="Löschen" onClick={() => remove.mutate(target.id)}>
        <Trash2 className="size-4" />
      </Button>
    </li>
  )
}

function SmtpSection() {
  const smtp = useSmtpSettings()
  const update = useUpdateSmtpSettings()
  const [draft, setDraft] = useState<Record<string, string> | null>(null)
  const [saved, setSaved] = useState(false)
  if (!smtp.data) return null
  const s = smtp.data
  const values = draft ?? {
    host: s.host ?? '',
    port: s.port ? String(s.port) : '',
    security: s.security,
    username: s.username ?? '',
    password: '',
    from_address: s.from_address ?? '',
    from_name: s.from_name ?? '',
  }
  const set = (key: string, value: string) => {
    setSaved(false)
    setDraft({ ...values, [key]: value })
  }

  function submit(event: FormEvent) {
    event.preventDefault()
    update.mutate(
      {
        host: values.host,
        port: values.port ? Number(values.port) : undefined,
        security: values.security as Schemas['SmtpSecurity'],
        username: values.username,
        password: values.password || undefined,
        from_address: values.from_address,
        from_name: values.from_name,
      },
      {
        onSuccess: () => {
          setDraft(null)
          setSaved(true)
        },
      },
    )
  }

  return (
    <Section title="E-Mail-Versand (SMTP)" description="Server, über den E-Mail-Benachrichtigungen verschickt werden.">
      <form onSubmit={submit} className="grid gap-4 md:grid-cols-2">
        <TextField label="SMTP-Server" placeholder="smtp.example.org" value={values.host} onChange={(e) => set('host', e.target.value)} />
        <TextField label="Port" inputMode="numeric" placeholder="587" value={values.port} onChange={(e) => set('port', e.target.value)} />
        <SelectField
          label="Verschlüsselung"
          value={values.security ?? 'starttls'}
          onChange={(v) => set('security', v)}
          options={[
            { value: 'starttls', label: 'STARTTLS (Port 587)' },
            { value: 'tls', label: 'TLS (Port 465)' },
            { value: 'none', label: 'Keine (nur im LAN)' },
          ]}
        />
        <TextField label="Benutzername" autoComplete="off" value={values.username} onChange={(e) => set('username', e.target.value)} />
        <TextField
          label={s.has_password ? 'Neues Passwort (gespeichert)' : 'Passwort'}
          type="password"
          autoComplete="new-password"
          value={values.password}
          onChange={(e) => set('password', e.target.value)}
        />
        <TextField label="Absenderadresse" type="email" value={values.from_address} onChange={(e) => set('from_address', e.target.value)} />
        <TextField label="Absendername" placeholder="Flixnet" value={values.from_name} onChange={(e) => set('from_name', e.target.value)} />
        <div className="flex flex-col justify-end gap-2 md:col-span-2">
          <FormError error={update.error} />
          {saved && (
            <p role="status" className="text-sm text-emerald-400">
              Gespeichert.
            </p>
          )}
          <Button type="submit" className="self-start" loading={update.isPending}>
            Speichern
          </Button>
        </div>
      </form>
    </Section>
  )
}

/** Webhooks, ntfy, Discord and e-mail for server events (administrators). */
export function NotificationsSettings() {
  const targets = useNotificationTargets()
  // `undefined`: closed; `null`: new target.
  const [editing, setEditing] = useState<NotificationTarget | null | undefined>(undefined)

  return (
    <div className="flex flex-col gap-6">
      <Section
        title="Benachrichtigungen"
        description="Flixnet meldet Ereignisse wie neue Filme, Wiedergaben oder Fehler an Webhooks, ntfy, Discord oder per E-Mail. Fehlgeschlagene Zustellungen werden automatisch wiederholt."
        action={
          <Button variant="accent" className="h-9 px-4" onClick={() => setEditing(null)}>
            <Plus className="size-4" aria-hidden="true" />
            Ziel hinzufügen
          </Button>
        }
      >
        {targets.data?.length === 0 && <p className="text-sm text-muted">Noch keine Ziele eingerichtet.</p>}
        <ul className="divide-y divide-line/60" data-testid="notification-targets">
          {targets.data?.map((t) => (
            <TargetRow key={t.id} target={t} onEdit={() => setEditing(t)} />
          ))}
        </ul>
      </Section>
      <SmtpSection />
      {editing !== undefined && <TargetDialog key={editing?.id ?? 'new'} target={editing} onClose={() => setEditing(undefined)} />}
    </div>
  )
}
