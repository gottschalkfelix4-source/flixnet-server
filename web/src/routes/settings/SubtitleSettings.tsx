import { CheckCircle2, ExternalLink, KeyRound } from 'lucide-react'
import { type FormEvent, useState } from 'react'
import { useSubtitleSettings, useUpdateSubtitleSettings } from '@/api/queries/subtitles'
import { FormError } from '@/components/layout/AuthLayout'
import { SUBTITLE_LANGUAGES } from '@/lib/subtitleLanguages'
import { Button } from '@/components/ui/Button'
import { Checkbox, Section } from '@/components/ui/Section'
import { TextField } from '@/components/ui/TextField'

export function SubtitleSettings() {
  const settings = useSubtitleSettings()
  const update = useUpdateSubtitleSettings()
  const [key, setKey] = useState('')
  const [username, setUsername] = useState<string | null>(null)
  const [password, setPassword] = useState('')
  const [saved, setSaved] = useState(false)

  if (!settings.data) return null
  const s = settings.data
  const shownUsername = username ?? s.username ?? ''

  function saveCredentials(event: FormEvent) {
    event.preventDefault()
    setSaved(false)
    update.mutate(
      {
        ...(key.trim() ? { api_key: key.trim() } : {}),
        ...(username !== null ? { username: username.trim() || null } : {}),
        ...(password ? { password } : {}),
      },
      {
        onSuccess: () => {
          setKey('')
          setPassword('')
          setUsername(null)
          setSaved(true)
        },
      },
    )
  }

  function toggleLanguage(code: string, on: boolean) {
    const languages = on ? [...s.languages, code] : s.languages.filter((l) => l !== code)
    update.mutate({ languages, ...(languages.length === 0 ? { auto_download: false } : {}) })
  }

  const changed = !!key.trim() || !!password || (username !== null && username.trim() !== (s.username ?? ''))

  return (
    <div className="flex flex-col gap-6">
      <Section
        title="OpenSubtitles.com"
        description="Untertitel suchen und herunterladen. Den API-Schlüssel gibt es kostenlos nach der Registrierung als Entwickler; mit einem Konto sind mehr Downloads pro Tag möglich."
      >
        <p className="mb-4 flex items-center gap-2 text-sm" data-testid="opensubtitles-status">
          {s.has_api_key ? (
            <>
              <CheckCircle2 className="size-4 text-emerald-400" aria-hidden="true" />
              Schlüssel hinterlegt{s.username ? ` – angemeldet als ${s.username}` : ' – ohne Konto (wenige Downloads pro Tag)'}.
            </>
          ) : (
            <>
              <KeyRound className="size-4 text-muted" aria-hidden="true" />
              Kein Schlüssel – die Untertitelsuche ist aus.
            </>
          )}
        </p>
        <form onSubmit={saveCredentials} className="grid max-w-xl gap-3">
          <TextField
            label={s.has_api_key ? 'Neuer API-Schlüssel' : 'API-Schlüssel'}
            type="password"
            autoComplete="off"
            value={key}
            onChange={(e) => setKey(e.target.value)}
          />
          <div className="grid gap-3 md:grid-cols-2">
            <TextField
              label="Benutzername (optional)"
              autoComplete="off"
              value={shownUsername}
              onChange={(e) => setUsername(e.target.value)}
            />
            <TextField
              label={s.has_password ? 'Neues Passwort' : 'Passwort (optional)'}
              type="password"
              autoComplete="new-password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
            />
          </div>
          <div className="flex flex-wrap gap-3">
            <Button type="submit" variant="accent" loading={update.isPending && changed} disabled={!changed}>
              Prüfen & speichern
            </Button>
            {s.has_api_key && (
              <Button
                type="button"
                variant="ghost"
                onClick={() => update.mutate({ api_key: null, username: null, password: null, auto_download: false })}
              >
                Entfernen
              </Button>
            )}
          </div>
        </form>
        <div className="mt-3">
          <FormError error={update.error} />
          {saved && (
            <p role="status" className="text-sm text-emerald-400">
              Gespeichert. Auf Film- und Folgenseiten gibt es jetzt „Untertitel suchen“.
            </p>
          )}
        </div>
        <a
          href="https://www.opensubtitles.com/de/consumers"
          target="_blank"
          rel="noreferrer"
          className="mt-4 inline-flex items-center gap-1.5 text-sm font-semibold text-accent hover:text-accent-hover"
        >
          API-Schlüssel bei OpenSubtitles erstellen
          <ExternalLink className="size-3.5" aria-hidden="true" />
        </a>
      </Section>

      <Section
        title="Automatisch herunterladen"
        description="Für neu hinzugefügte Filme und Folgen stündlich Untertitel in den gewählten Sprachen laden, wenn noch keine vorhanden sind. Höchstens 10 pro Durchlauf, damit das Tageskontingent reicht."
      >
        <fieldset className="mb-4 flex flex-wrap gap-2" aria-label="Sprachen">
          {SUBTITLE_LANGUAGES.slice(0, 12).map((l) => {
            const on = s.languages.includes(l.value)
            return (
              <button
                key={l.value}
                type="button"
                aria-pressed={on}
                onClick={() => toggleLanguage(l.value, !on)}
                className={
                  on
                    ? 'rounded-full border border-accent bg-accent/15 px-3 py-1 text-xs font-semibold text-fg'
                    : 'rounded-full border border-line px-3 py-1 text-xs font-semibold text-muted hover:text-fg'
                }
              >
                {l.label}
              </button>
            )
          })}
        </fieldset>
        <Checkbox
          label="Untertitel für neue Inhalte automatisch laden"
          hint={
            !s.has_api_key
              ? 'Zuerst einen API-Schlüssel eintragen.'
              : s.languages.length === 0
                ? 'Zuerst mindestens eine Sprache wählen.'
                : 'Gilt für alles, was ab dem Einschalten neu in die Bibliotheken kommt.'
          }
          checked={s.auto_download}
          onChange={(on) => {
            if (!on || (s.has_api_key && s.languages.length > 0)) update.mutate({ auto_download: on })
          }}
        />
      </Section>
    </div>
  )
}
