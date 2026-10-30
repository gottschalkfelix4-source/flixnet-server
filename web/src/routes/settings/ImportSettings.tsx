import { CheckCircle2, Download, Search } from 'lucide-react'
import { type FormEvent, useState } from 'react'
import { type ImportRequest, type ImportResult, useImport } from '@/api/queries/ops'
import { useUsers } from '@/api/queries/users'
import { FormError } from '@/components/layout/AuthLayout'
import { Button } from '@/components/ui/Button'
import { Section, SelectField } from '@/components/ui/Section'
import { TextField } from '@/components/ui/TextField'

type Source = ImportRequest['source']

function Stats({ user }: { user: ImportResult['users'][number] }) {
  const s = user.stats
  const share = s.entries ? Math.round((s.matched / s.entries) * 100) : 0
  return (
    <div className="text-sm">
      <p>
        <span className="font-semibold">{s.matched}</span> von {s.entries} Einträgen gefunden ({share} %) ·{' '}
        <span className="text-muted">
          {s.by_provider_id} per ID, {s.by_title_year} per Titel/Jahr, {s.by_episode} per Staffel/Folge
        </span>
      </p>
      <p className="text-xs text-subtle">
        {s.played} gesehen · {s.in_progress} angefangen · {s.favorites} Favoriten · {s.rated} bewertet
      </p>
      {user.unmatched_samples.length > 0 && (
        <details className="mt-1 text-xs text-muted">
          <summary className="cursor-pointer">{s.unmatched} nicht gefunden</summary>
          <ul className="mt-1 list-disc pl-5">
            {user.unmatched_samples.map((t) => (
              <li key={t}>{t}</li>
            ))}
          </ul>
        </details>
      )}
      {user.written_items != null && (
        <p className="mt-1 flex items-center gap-1.5 text-xs text-emerald-400">
          <CheckCircle2 className="size-3.5" aria-hidden="true" />
          {user.written_items} Einträge übernommen, {user.written_history} Verlaufseinträge ergänzt.
        </p>
      )}
    </div>
  )
}

/** Watch history import from Plex or Jellyfin, with a dry run first. */
export function ImportSettings() {
  const users = useUsers()
  const run = useImport()
  const [source, setSource] = useState<Source>('plex')
  const [serverUrl, setServerUrl] = useState('')
  const [token, setToken] = useState('')
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [userId, setUserId] = useState('')
  const [mapping, setMapping] = useState<Record<string, string>>({})

  const options = (users.data ?? []).map((u) => ({ value: String(u.id), label: u.name }))
  const targetUser = userId || options[0]?.value || ''
  const usesApiKey = source === 'jellyfin' && token.trim() !== ''

  function request(): ImportRequest {
    return {
      source,
      server_url: serverUrl.trim(),
      token: token.trim() || undefined,
      username: source === 'jellyfin' && !usesApiKey ? username.trim() || undefined : undefined,
      password: source === 'jellyfin' && !usesApiKey ? password : undefined,
      user_id: usesApiKey ? undefined : Number(targetUser),
      // 0 = deliberately not imported (no Flixnet user has id 0).
      user_map: Object.entries(mapping).map(([source_user_id, id]) => ({ source_user_id, user_id: Number(id || 0) })),
    }
  }

  function preview(event: FormEvent) {
    event.preventDefault()
    run.mutate({ apply: false, body: request() })
  }

  const result = run.data
  const applying = run.isPending && run.variables?.apply === true
  const previewing = run.isPending && !applying

  return (
    <div className="flex flex-col gap-6">
      <Section
        title="Verlauf importieren"
        description="Übernimmt Gesehen-Status, Fortschritt, Favoriten und Bewertungen aus Plex oder Jellyfin. Zuerst eine Vorschau, dann importieren; ein zweiter Import legt nichts doppelt an."
      >
        <form onSubmit={preview} className="grid gap-4 md:grid-cols-2">
          <SelectField
            label="Quelle"
            value={source}
            onChange={(v) => {
              setSource(v as Source)
              run.reset()
            }}
            options={[
              { value: 'plex', label: 'Plex' },
              { value: 'jellyfin', label: 'Jellyfin' },
            ]}
          />
          <TextField
            label="Server-Adresse"
            type="url"
            required
            placeholder={source === 'plex' ? 'http://192.168.1.10:32400' : 'http://192.168.1.10:8096'}
            value={serverUrl}
            onChange={(e) => setServerUrl(e.target.value)}
          />
          <TextField
            label={source === 'plex' ? 'Plex-Token' : 'API-Schlüssel (alle Nutzer)'}
            type="password"
            autoComplete="off"
            required={source === 'plex'}
            hint={
              source === 'plex'
                ? 'X-Plex-Token, z. B. aus „XML anzeigen“ eines Eintrags in Plex Web.'
                : 'Jellyfin → Dashboard → API-Schlüssel. Alternativ unten mit Name und Passwort.'
            }
            value={token}
            onChange={(e) => setToken(e.target.value)}
          />
          {source === 'jellyfin' && !usesApiKey && (
            <>
              <TextField label="Jellyfin-Name" autoComplete="off" value={username} onChange={(e) => setUsername(e.target.value)} />
              <TextField
                label="Jellyfin-Passwort"
                type="password"
                autoComplete="off"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              />
            </>
          )}
          {!usesApiKey && (
            <SelectField label="In Flixnet-Profil" value={targetUser} onChange={setUserId} options={options} />
          )}
          <div className="flex items-end md:col-span-2">
            <Button type="submit" loading={previewing} disabled={!serverUrl.trim()}>
              <Search className="size-4" aria-hidden="true" />
              Vorschau
            </Button>
          </div>
        </form>
        <div className="mt-3">
          <FormError error={run.error} />
        </div>
      </Section>

      {result && (
        <Section
          title={result.applied ? 'Import abgeschlossen' : 'Vorschau'}
          description={result.applied ? undefined : 'Noch wurde nichts geschrieben.'}
          action={
            !result.applied && (
              <Button
                variant="accent"
                className="h-9 px-4"
                loading={applying}
                onClick={() => run.mutate({ apply: true, body: request() })}
              >
                <Download className="size-4" aria-hidden="true" />
                Jetzt importieren
              </Button>
            )
          }
        >
          <ul className="divide-y divide-line/60" data-testid="import-users">
            {result.users.map((u) => (
              <li key={u.source_user_id} className="flex flex-col gap-2 py-3">
                <div className="flex flex-wrap items-center gap-3">
                  <p className="font-semibold">{u.source_user_name}</p>
                  <span className="text-muted">→</span>
                  {usesApiKey && !result.applied ? (
                    <select
                      aria-label={`Flixnet-Profil für ${u.source_user_name}`}
                      value={mapping[u.source_user_id] ?? (u.user_id != null ? String(u.user_id) : '')}
                      onChange={(e) => setMapping({ ...mapping, [u.source_user_id]: e.target.value })}
                      className="h-9 rounded-lg border border-line bg-surface-2 px-2 text-sm"
                    >
                      <option value="">Nicht importieren</option>
                      {options.map((o) => (
                        <option key={o.value} value={o.value}>
                          {o.label}
                        </option>
                      ))}
                    </select>
                  ) : (
                    <span className="text-sm">{u.user_name ?? 'nicht importiert'}</span>
                  )}
                </div>
                <Stats user={u} />
              </li>
            ))}
          </ul>
        </Section>
      )}
    </div>
  )
}
