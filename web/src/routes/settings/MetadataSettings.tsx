import { CheckCircle2, ExternalLink, KeyRound } from 'lucide-react'
import { type FormEvent, useState } from 'react'
import { useMetadataSettings, useUpdateMetadataSettings } from '@/api/queries/settings'
import { FormError } from '@/components/layout/AuthLayout'
import { Button } from '@/components/ui/Button'
import { Section, SelectField } from '@/components/ui/Section'
import { TextField } from '@/components/ui/TextField'

const LANGUAGES = [
  { value: 'de-DE', label: 'Deutsch' },
  { value: 'en-US', label: 'Englisch' },
  { value: 'fr-FR', label: 'Französisch' },
  { value: 'es-ES', label: 'Spanisch' },
  { value: 'it-IT', label: 'Italienisch' },
]
const COUNTRIES = [
  { value: 'DE', label: 'Deutschland (FSK)' },
  { value: 'AT', label: 'Österreich' },
  { value: 'CH', label: 'Schweiz' },
  { value: 'US', label: 'USA (MPAA)' },
  { value: 'GB', label: 'Großbritannien (BBFC)' },
]

function FanartSection({ hasKey }: { hasKey: boolean }) {
  const update = useUpdateMetadataSettings()
  const [key, setKey] = useState('')

  return (
    <Section
      title="fanart.tv (optional)"
      description="Liefert besonders schöne Titel-Logos, Banner und Hintergründe. Benötigt einen kostenlosen persönlichen Schlüssel."
    >
      <form
        onSubmit={(e: FormEvent) => {
          e.preventDefault()
          update.mutate({ fanart_api_key: key.trim() }, { onSuccess: () => setKey('') })
        }}
        className="flex max-w-xl flex-wrap items-end gap-3"
      >
        <TextField
          className="min-w-64 flex-1"
          label={hasKey ? 'Neuer Schlüssel (gespeichert)' : 'Persönlicher API-Schlüssel'}
          type="password"
          autoComplete="off"
          value={key}
          onChange={(e) => setKey(e.target.value)}
        />
        <Button type="submit" loading={update.isPending && !!key} disabled={!key.trim()}>
          Speichern
        </Button>
        {hasKey && (
          <Button type="button" variant="ghost" onClick={() => update.mutate({ fanart_api_key: null })}>
            Entfernen
          </Button>
        )}
      </form>
      <a
        href="https://fanart.tv/get-an-api-key/"
        target="_blank"
        rel="noreferrer"
        className="mt-4 inline-flex items-center gap-1.5 text-sm font-semibold text-accent hover:text-accent-hover"
      >
        Schlüssel bei fanart.tv holen
        <ExternalLink className="size-3.5" aria-hidden="true" />
      </a>
    </Section>
  )
}

export function MetadataSettings() {
  const settings = useMetadataSettings()
  const update = useUpdateMetadataSettings()
  const [key, setKey] = useState('')
  const [saved, setSaved] = useState(false)

  function saveKey(event: FormEvent) {
    event.preventDefault()
    setSaved(false)
    update.mutate({ tmdb_api_key: key.trim() }, { onSuccess: () => { setKey(''); setSaved(true) } })
  }

  if (!settings.data) return null
  const { has_tmdb_api_key, language, country } = settings.data

  return (
    <div className="flex flex-col gap-6">
      <Section
        title="The Movie Database (TMDB)"
        description="Für Titel, Beschreibungen, Poster, Darsteller und Altersfreigaben. Der Schlüssel ist kostenlos."
      >
        <p className="mb-4 flex items-center gap-2 text-sm" data-testid="tmdb-status">
          {has_tmdb_api_key ? (
            <>
              <CheckCircle2 className="size-4 text-emerald-400" aria-hidden="true" />
              Schlüssel hinterlegt – Online-Metadaten sind aktiv.
            </>
          ) : (
            <>
              <KeyRound className="size-4 text-muted" aria-hidden="true" />
              Kein Schlüssel – es werden nur lokale NFO-Dateien und Bilder verwendet.
            </>
          )}
        </p>
        <form onSubmit={saveKey} className="flex max-w-xl flex-wrap items-end gap-3">
          <TextField
            className="min-w-64 flex-1"
            label={has_tmdb_api_key ? 'Neuer API-Schlüssel' : 'API-Schlüssel oder Read Access Token'}
            type="password"
            autoComplete="off"
            value={key}
            onChange={(e) => setKey(e.target.value)}
          />
          <Button type="submit" variant="accent" loading={update.isPending && !!key} disabled={!key.trim()}>
            Prüfen & speichern
          </Button>
          {has_tmdb_api_key && (
            <Button type="button" variant="ghost" onClick={() => update.mutate({ tmdb_api_key: null })}>
              Entfernen
            </Button>
          )}
        </form>
        <div className="mt-3">
          <FormError error={update.error} />
          {saved && (
            <p role="status" className="text-sm text-emerald-400">
              Schlüssel gespeichert. Neue und aktualisierte Titel werden jetzt online ergänzt.
            </p>
          )}
        </div>
        <a
          href="https://www.themoviedb.org/settings/api"
          target="_blank"
          rel="noreferrer"
          className="mt-4 inline-flex items-center gap-1.5 text-sm font-semibold text-accent hover:text-accent-hover"
        >
          Schlüssel bei TMDB erstellen
          <ExternalLink className="size-3.5" aria-hidden="true" />
        </a>
      </Section>

      <FanartSection hasKey={settings.data.has_fanart_api_key} />

      <Section title="Sprache & Region">
        <div className="grid max-w-xl gap-4 md:grid-cols-2">
          <SelectField
            label="Sprache der Metadaten"
            value={language}
            onChange={(value) => update.mutate({ language: value })}
            options={LANGUAGES}
          />
          <SelectField
            label="Altersfreigaben aus"
            value={country}
            onChange={(value) => update.mutate({ country: value })}
            options={COUNTRIES}
          />
        </div>
      </Section>
    </div>
  )
}
