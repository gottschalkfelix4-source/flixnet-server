import { CheckCircle2, CircleOff } from 'lucide-react'
import { type FormEvent, useState } from 'react'
import {
  useLlmSettings,
  useSceneSettings,
  useTestLlm,
  useUpdateLlmSettings,
  useUpdateSceneSettings,
} from '@/api/queries/assistant'
import { useRunTask } from '@/api/queries/jobs'
import type { Schemas } from '@/api/client'
import { FormError } from '@/components/layout/AuthLayout'
import { Button } from '@/components/ui/Button'
import { Checkbox, Section, SelectField } from '@/components/ui/Section'
import { TextField } from '@/components/ui/TextField'
import { EmbeddingSettings } from './EmbeddingSettings'

export function AiSettings() {
  return (
    <div className="flex flex-col gap-6">
      {/* Local embeddings first: semantic search and recommendations need no LLM. */}
      <EmbeddingSettings />
      <LlmSection />
      <SceneSection />
    </div>
  )
}

function LlmSection() {
  const settings = useLlmSettings()
  if (!settings.data) return null
  // Keyed: the form starts from the stored values again after saving.
  return <LlmForm key={JSON.stringify(settings.data)} s={settings.data} />
}

function LlmForm({ s }: { s: Schemas['LlmSettingsDto'] }) {
  const update = useUpdateLlmSettings()
  const test = useTestLlm()
  const runTask = useRunTask()
  const [baseUrl, setBaseUrl] = useState(s.base_url ?? '')
  const [model, setModel] = useState(s.model ?? '')
  const [key, setKey] = useState('')
  const [timeout, setTimeoutSecs] = useState(String(s.timeout_secs))
  const [enabled, setEnabled] = useState(s.enabled)
  const [themesQueued, setThemesQueued] = useState(false)

  function save(event: FormEvent) {
    event.preventDefault()
    test.reset()
    update.mutate({
      enabled,
      base_url: baseUrl.trim() || null,
      model: model.trim() || null,
      ...(key.trim() ? { api_key: key.trim() } : {}),
      timeout_secs: Number(timeout) || undefined,
    })
  }

  return (
    <Section
      title="Sprachmodell (optional)"
      description="Ein OpenAI-kompatibler Endpunkt – lokal mit Ollama oder LM Studio, oder ein Cloud-Dienst. Damit werden Suchanfragen in eigenen Worten verstanden, Empfehlungen begründet, Zusammenfassungen geschrieben und der Assistent „Frag Flixnet“ freigeschaltet. Ohne Sprachmodell funktionieren Suche und Szenensuche weiter."
    >
      <p className="mb-4 flex items-center gap-2 text-sm" data-testid="llm-status">
        {s.configured ? (
          <>
            <CheckCircle2 className="size-4 text-emerald-400" aria-hidden="true" />
            Aktiv: {s.model} über {s.base_url}
          </>
        ) : (
          <>
            <CircleOff className="size-4 text-muted" aria-hidden="true" />
            Kein Sprachmodell aktiv.
          </>
        )}
      </p>
      <form onSubmit={save} className="grid max-w-xl gap-3">
        <TextField
          label="Adresse"
          placeholder="http://localhost:11434/v1"
          hint="Ollama: http://<host>:11434/v1 · LM Studio: http://<host>:1234/v1 · OpenAI: https://api.openai.com/v1"
          value={baseUrl}
          onChange={(e) => setBaseUrl(e.target.value)}
        />
        <div className="grid gap-3 md:grid-cols-2">
          <TextField label="Modell" placeholder="z. B. llama3.1:8b" value={model} onChange={(e) => setModel(e.target.value)} />
          <TextField
            label="Zeitgrenze (Sekunden)"
            type="number"
            min={5}
            max={900}
            value={timeout}
            onChange={(e) => setTimeoutSecs(e.target.value)}
          />
        </div>
        <TextField
          label={s.has_api_key ? 'Neuer API-Schlüssel' : 'API-Schlüssel (optional)'}
          type="password"
          autoComplete="off"
          value={key}
          onChange={(e) => setKey(e.target.value)}
        />
        <Checkbox label="Sprachmodell verwenden" checked={enabled} onChange={setEnabled} />
        <div className="flex flex-wrap gap-3">
          <Button type="submit" variant="accent" loading={update.isPending}>
            Speichern
          </Button>
          <Button type="button" variant="ghost" loading={test.isPending} disabled={!s.base_url || !s.model} onClick={() => test.mutate()}>
            Verbindung testen
          </Button>
          {s.has_api_key && (
            <Button type="button" variant="ghost" onClick={() => update.mutate({ api_key: null })}>
              Schlüssel entfernen
            </Button>
          )}
        </div>
      </form>
      <div className="mt-3 flex flex-col gap-2">
        <FormError error={update.error ?? test.error} />
        {test.data && (
          <p role="status" className={test.data.ok ? 'text-sm text-emerald-400' : 'text-sm text-red-300'} data-testid="llm-test">
            {test.data.ok ? `Antwort nach ${(test.data.duration_ms / 1000).toFixed(1)} s: „${test.data.message}“` : test.data.message}
          </p>
        )}
      </div>
      <div className="mt-6 border-t border-line/70 pt-5">
        <h3 className="font-bold">Themen-Sammlungen</h3>
        <p className="mt-1 mb-3 text-sm text-muted">
          Das Sprachmodell schlägt Themen über die ganze Bibliothek vor („Zeitreisen“, „Heist-Filme“) und legt sie als Sammlungen an. Läuft
          wöchentlich; jedes Profil sieht nur die Titel, die es sehen darf.
        </p>
        <Button
          variant="ghost"
          disabled={!s.configured}
          loading={runTask.isPending}
          onClick={() => runTask.mutate('theme_collections', { onSuccess: () => setThemesQueued(true) })}
        >
          Jetzt erzeugen
        </Button>
        {themesQueued && (
          <p role="status" className="mt-2 text-sm text-emerald-400">
            Wird im Hintergrund erstellt (siehe „Aufgaben“).
          </p>
        )}
      </div>
    </Section>
  )
}

function SceneSection() {
  const settings = useSceneSettings()
  if (!settings.data) return null
  return <SceneForm key={JSON.stringify(settings.data)} s={settings.data} />
}

const LANGUAGES = [
  { value: 'auto', label: 'Automatisch erkennen' },
  { value: 'de', label: 'Deutsch' },
  { value: 'en', label: 'Englisch' },
  { value: 'fr', label: 'Französisch' },
  { value: 'es', label: 'Spanisch' },
  { value: 'it', label: 'Italienisch' },
]

function SceneForm({ s }: { s: Schemas['SceneSettingsDto'] }) {
  const update = useUpdateSceneSettings()
  const runTask = useRunTask()
  const [path, setPath] = useState(s.whisper_path ?? '')
  const [model, setModel] = useState(s.model)
  const [language, setLanguage] = useState(s.language)
  const [threads, setThreads] = useState(String(s.threads))
  const [libraries, setLibraries] = useState(s.libraries.filter((l) => l.transcribe).map((l) => l.id))
  const [queued, setQueued] = useState(false)

  function save(event: FormEvent) {
    event.preventDefault()
    update.mutate({
      whisper_path: path.trim() || null,
      model,
      language,
      threads: Number(threads) || undefined,
      transcribe_library_ids: libraries,
    })
  }

  return (
    <Section
      title="Szenensuche"
      description="Die Dialoge aller Untertitel werden in Abschnitte von 30–60 Sekunden zerlegt und durchsuchbar gemacht – „die Szene, wo sie am Strand streiten“ springt direkt an die Stelle. Läuft im Hintergrund, nie während einer Transkodierung."
      action={
        <Button
          variant="ghost"
          loading={runTask.isPending}
          onClick={() => runTask.mutate(s.task, { onSuccess: () => setQueued(true) })}
        >
          Index aktualisieren
        </Button>
      }
    >
      <p className="mb-5 text-sm" data-testid="scene-stats">
        {s.indexed_files} {s.indexed_files === 1 ? 'Datei' : 'Dateien'} mit {s.chunks} Dialogabschnitten indexiert
        {s.whisper_files > 0 && `, davon ${s.whisper_files} per Whisper`}.
        {queued && <span className="ml-2 text-emerald-400">Aktualisierung gestartet.</span>}
      </p>
      <form onSubmit={save} className="grid max-w-xl gap-3">
        <h3 className="font-bold">Whisper für Dateien ohne Untertitel (Opt-in)</h3>
        <p className="-mt-2 text-sm text-muted">
          Braucht das Programm <code>whisper-cli</code> aus whisper.cpp. Das Modell wird beim ersten Einsatz nach <code>/config/models</code>{' '}
          geladen und per SHA-256 geprüft. Transkribieren ist langsam und läuft nur für die gewählten Bibliotheken.
        </p>
        <TextField
          label="Pfad zu whisper-cli"
          placeholder="/usr/local/bin/whisper-cli"
          hint="Leer lassen, um Whisper auszuschalten."
          value={path}
          onChange={(e) => setPath(e.target.value)}
        />
        <div className="grid gap-3 md:grid-cols-3">
          <SelectField
            label="Modell"
            value={model}
            onChange={setModel}
            options={s.models.map((m) => ({ value: m.name, label: `${m.name} (${Math.round(m.size / 1_000_000)} MB)` }))}
          />
          <SelectField label="Sprache" value={language} onChange={setLanguage} options={LANGUAGES} />
          <TextField label="Threads" type="number" min={1} max={64} value={threads} onChange={(e) => setThreads(e.target.value)} />
        </div>
        {s.libraries.length > 0 && (
          <fieldset className="flex flex-col gap-2" aria-label="Bibliotheken transkribieren">
            <legend className="mb-2 text-sm font-semibold text-muted">Bibliotheken transkribieren</legend>
            {s.libraries.map((l) => (
              <Checkbox
                key={l.id}
                label={l.name}
                checked={libraries.includes(l.id)}
                onChange={(on) => setLibraries((ids) => (on ? [...ids, l.id] : ids.filter((id) => id !== l.id)))}
              />
            ))}
          </fieldset>
        )}
        <div>
          <Button type="submit" variant="accent" loading={update.isPending}>
            Speichern
          </Button>
        </div>
        <FormError error={update.error} />
      </form>
    </Section>
  )
}
