import { CheckCircle2, ExternalLink, RefreshCw, Unplug } from 'lucide-react'
import { type FormEvent, useEffect, useState } from 'react'
import {
  useDisconnectTrakt,
  usePollTraktConnect,
  useStartTraktConnect,
  useSyncTrakt,
  useTraktSettings,
  useTraktStatus,
  useUpdateTraktOptions,
  useUpdateTraktSettings,
} from '@/api/queries/ops'
import { FormError } from '@/components/layout/AuthLayout'
import { Button } from '@/components/ui/Button'
import { Checkbox, Section } from '@/components/ui/Section'
import { TextField } from '@/components/ui/TextField'
import { formatDateTime } from '@/lib/format'
import { useSession } from '@/lib/session'

/** The administrator registers the Trakt API app once. */
function AppSection() {
  const settings = useTraktSettings(true)
  const update = useUpdateTraktSettings()
  const [clientId, setClientId] = useState<string | null>(null)
  const [secret, setSecret] = useState('')
  if (!settings.data) return null
  const s = settings.data

  function submit(event: FormEvent) {
    event.preventDefault()
    update.mutate(
      { client_id: clientId ?? s.client_id ?? '', client_secret: secret || undefined },
      {
        onSuccess: () => {
          setClientId(null)
          setSecret('')
        },
      },
    )
  }

  return (
    <Section
      title="Trakt-App (Administrator)"
      description="Einmalig eine eigene API-App bei Trakt anlegen und hier eintragen; danach kann jedes Profil sein Trakt-Konto verbinden."
    >
      <form onSubmit={submit} className="grid gap-4 md:grid-cols-2">
        <TextField
          label="Client ID"
          autoComplete="off"
          value={clientId ?? s.client_id ?? ''}
          onChange={(e) => setClientId(e.target.value)}
        />
        <TextField
          label={s.has_client_secret ? 'Neues Client Secret (gespeichert)' : 'Client Secret'}
          type="password"
          autoComplete="off"
          value={secret}
          onChange={(e) => setSecret(e.target.value)}
        />
        <p className="text-xs text-subtle md:col-span-2">
          Als Redirect URI <code className="text-fg">{s.redirect_uri}</code> angeben.
        </p>
        <div className="flex flex-wrap items-center gap-3 md:col-span-2">
          <Button type="submit" loading={update.isPending}>
            Speichern
          </Button>
          <a
            href="https://trakt.tv/oauth/applications/new"
            target="_blank"
            rel="noreferrer"
            className="inline-flex items-center gap-1.5 text-sm font-semibold text-accent hover:text-accent-hover"
          >
            App bei Trakt anlegen
            <ExternalLink className="size-3.5" aria-hidden="true" />
          </a>
        </div>
        <FormError error={update.error} />
      </form>
    </Section>
  )
}

function Connect({ pending }: { pending: boolean }) {
  const start = useStartTraktConnect()
  const poll = usePollTraktConnect()
  const status = useTraktStatus()
  const code = status.data?.pending ?? start.data
  const interval = code?.interval ?? 5
  const state = poll.data?.state
  const waiting = (pending || start.data != null) && state !== 'expired' && state !== 'denied'
  const { mutate: pollNow } = poll

  // Ask the server until the code is confirmed (or expires); the server talks to Trakt.
  useEffect(() => {
    if (!waiting) return
    const timer = window.setInterval(() => pollNow(), interval * 1000)
    return () => window.clearInterval(timer)
  }, [waiting, interval, pollNow])

  return (
    <div className="flex flex-col gap-4">
      {code && state !== 'expired' && state !== 'denied' ? (
        <div className="rounded-xl border border-accent/40 bg-accent/10 p-4" data-testid="trakt-code">
          <p className="text-sm text-muted">Öffne die Adresse und gib diesen Code ein:</p>
          <p className="mt-2 font-mono text-3xl font-extrabold tracking-widest">{code.user_code}</p>
          <a
            href={code.verification_url}
            target="_blank"
            rel="noreferrer"
            className="mt-2 inline-flex items-center gap-1.5 text-sm font-semibold text-accent hover:text-accent-hover"
          >
            {code.verification_url}
            <ExternalLink className="size-3.5" aria-hidden="true" />
          </a>
          <p className="mt-2 text-xs text-subtle">Flixnet wartet auf die Bestätigung …</p>
        </div>
      ) : (
        <>
          {state === 'expired' && <p className="text-sm text-amber-400">Der Code ist abgelaufen. Bitte neu starten.</p>}
          {state === 'denied' && <p className="text-sm text-red-400">Die Verbindung wurde bei Trakt abgelehnt.</p>}
          <Button
            variant="accent"
            className="self-start"
            loading={start.isPending}
            onClick={() => {
              poll.reset()
              start.mutate()
            }}
          >
            Mit Trakt verbinden
          </Button>
        </>
      )}
      <FormError error={start.error ?? poll.error} />
    </div>
  )
}

/** Per-profile Trakt connection: scrobbling and sync of watched state and ratings. */
export function TraktSettings() {
  const session = useSession()
  const isAdmin = session?.user.is_admin ?? false
  const status = useTraktStatus()
  const options = useUpdateTraktOptions()
  const sync = useSyncTrakt()
  const disconnect = useDisconnectTrakt()
  const s = status.data

  return (
    <div className="flex flex-col gap-6">
      {s && (
        <Section
          title="Trakt"
          description="Meldet, was du schaust, an Trakt.tv und gleicht Gesehen-Status und Bewertungen in beide Richtungen ab."
        >
          {!s.configured && (
            <p className="text-sm text-muted">
              Trakt ist auf diesem Server noch nicht eingerichtet{isAdmin ? ' – zuerst unten die Trakt-App eintragen.' : '. Bitte einen Administrator fragen.'}
            </p>
          )}
          {s.configured && !s.connected && <Connect pending={s.pending != null} />}
          {s.connected && (
            <div className="flex flex-col gap-4">
              <p className="flex items-center gap-2 text-sm" data-testid="trakt-connected">
                <CheckCircle2 className="size-4 text-emerald-400" aria-hidden="true" />
                Verbunden{s.username ? ` als ${s.username}` : ''}.
              </p>
              <Checkbox
                label="Wiedergaben an Trakt melden (Scrobbling)"
                checked={s.scrobble}
                onChange={(v) => options.mutate({ scrobble: v })}
              />
              <Checkbox
                label="Bewertungen abgleichen"
                hint="Bei unterschiedlichen Bewertungen gilt die aus Flixnet."
                checked={s.sync_ratings}
                onChange={(v) => options.mutate({ sync_ratings: v })}
              />
              <p className="text-xs text-subtle">
                {s.last_sync_at ? `Zuletzt abgeglichen ${formatDateTime(s.last_sync_at)}` : 'Noch nicht abgeglichen'}
                {s.last_sync &&
                  ` · ${s.last_sync.pulled_watched} übernommen, ${s.last_sync.pushed_watched} gesendet, ${s.last_sync.unmatched} nicht in der Bibliothek`}
                . Automatisch alle 6 Stunden.
              </p>
              <div className="flex flex-wrap gap-3">
                <Button loading={sync.isPending} onClick={() => sync.mutate()}>
                  <RefreshCw className="size-4" aria-hidden="true" />
                  Jetzt abgleichen
                </Button>
                <Button variant="ghost" loading={disconnect.isPending} onClick={() => disconnect.mutate()}>
                  <Unplug className="size-4" aria-hidden="true" />
                  Trennen
                </Button>
              </div>
              <FormError error={sync.error ?? options.error} />
            </div>
          )}
        </Section>
      )}
      {isAdmin && <AppSection />}
    </div>
  )
}
