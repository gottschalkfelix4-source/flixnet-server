import { AlertTriangle, CheckCircle2, Download, LoaderCircle, RefreshCw, Trash2 } from 'lucide-react'
import { type FormEvent, useState } from 'react'
import {
  type AiStatus,
  useAiSettings,
  useAiStatus,
  useDeleteAiModel,
  useDownloadAiModel,
  useStartEmbedding,
  useUpdateAiSettings,
} from '@/api/queries/ai'
import { FormError } from '@/components/layout/AuthLayout'
import { Button } from '@/components/ui/Button'
import { Checkbox, Section, SelectField } from '@/components/ui/Section'
import { TextField } from '@/components/ui/TextField'

const EMBEDDERS = [
  { value: 'local', label: 'Lokal (multilingual-e5-base, ohne Cloud)' },
  { value: 'openai', label: 'OpenAI-kompatibler Endpoint (OpenAI, Ollama …)' },
  { value: 'off', label: 'Aus (nur Volltextsuche)' },
]

function megabytes(bytes: number) {
  return bytes > 0 ? `${Math.round(bytes / 1_000_000)} MB` : 'Größe unbekannt'
}

function Progress({ value }: { value: number | null | undefined }) {
  return (
    <div className="h-2 w-full max-w-md overflow-hidden rounded-full bg-surface-3" role="progressbar" aria-valuenow={Math.round((value ?? 0) * 100)}>
      <div className="h-full bg-accent transition-all" style={{ width: `${Math.round((value ?? 0) * 100)}%` }} />
    </div>
  )
}

function StatusSection({ status }: { status: AiStatus }) {
  const download = useDownloadAiModel()
  const remove = useDeleteAiModel()
  const embed = useStartEmbedding()
  const share = status.total > 0 ? status.embedded / status.total : 0

  return (
    <Section
      title="Status"
      description="Embeddings beschreiben jeden Film und jede Serie als Vektor. Darauf bauen die semantische Suche und die Empfehlungen auf."
    >
      <div className="flex flex-col gap-4 text-sm" data-testid="ai-status">
        {status.embedder === 'stub' && (
          <p className="flex items-center gap-2 text-amber-300">
            <AlertTriangle className="size-4" aria-hidden="true" />
            Test-Embedder aktiv (ai.embedder = stub) – nur für automatische Tests gedacht.
          </p>
        )}
        {status.embedder === 'off' ? (
          <p className="text-muted">KI ist ausgeschaltet. Die Suche arbeitet nur mit dem Volltext.</p>
        ) : (
          <>
            <p className="flex items-center gap-2">
              {status.ready ? (
                <CheckCircle2 className="size-4 text-emerald-400" aria-hidden="true" />
              ) : (
                <AlertTriangle className="size-4 text-amber-300" aria-hidden="true" />
              )}
              {status.ready ? 'Bereit' : 'Noch nicht bereit'}
              {status.model_key && <span className="text-subtle">· {status.model_key}</span>}
            </p>
            <div>
              <p className="mb-1.5 text-muted">
                {status.embedded} von {status.total} Titeln analysiert
              </p>
              <Progress value={share} />
            </div>
          </>
        )}

        {status.embedder === 'local' && !status.platform_supported && (
          <p className="text-amber-300">
            Für diese Plattform gibt es kein fertiges ONNX Runtime. Den Pfad zur Bibliothek in ai.onnxruntime_path eintragen.
          </p>
        )}
        {status.embedder === 'local' && status.missing.length > 0 && (
          <div className="flex flex-col gap-2">
            <p className="text-muted">
              Fehlende Dateien: {status.missing.map((m) => `${m.label} (${megabytes(m.size)})`).join(', ')}
            </p>
            {!status.download && (
              <Button
                className="self-start"
                variant="accent"
                loading={download.isPending}
                disabled={!status.platform_supported}
                onClick={() => download.mutate()}
              >
                <Download className="size-4" aria-hidden="true" />
                Modell herunterladen
              </Button>
            )}
          </div>
        )}
        {status.download && (
          <div>
            <p className="mb-1.5 flex items-center gap-2 text-muted">
              <LoaderCircle className="size-4 animate-spin" aria-hidden="true" />
              {status.download.state === 'queued' ? 'Download wartet …' : 'Modell wird heruntergeladen …'}
            </p>
            <Progress value={status.download.progress} />
          </div>
        )}
        {status.embedding && (
          <p className="flex items-center gap-2 text-muted">
            <LoaderCircle className="size-4 animate-spin" aria-hidden="true" />
            {status.embedding.state === 'queued'
              ? 'Analyse wartet (pausiert, solange jemand transkodiert schaut) …'
              : `Titel werden analysiert … ${Math.round((status.embedding.progress ?? 0) * 100)} %`}
          </p>
        )}
        {status.last_error && (
          <p role="alert" className="rounded-lg border border-red-500/30 bg-red-500/10 px-3 py-2 text-red-300">
            {status.last_error}
          </p>
        )}

        {status.embedder !== 'off' && (
          <div className="flex flex-wrap gap-2">
            <Button variant="ghost" loading={embed.isPending} onClick={() => embed.mutate(false)}>
              <RefreshCw className="size-4" aria-hidden="true" />
              Jetzt analysieren
            </Button>
            <Button variant="ghost" onClick={() => embed.mutate(true)}>
              Alles neu berechnen
            </Button>
            {status.embedder === 'local' && status.missing.length === 0 && (
              <Button variant="ghost" loading={remove.isPending} onClick={() => remove.mutate()}>
                <Trash2 className="size-4" aria-hidden="true" />
                Modelldateien löschen
              </Button>
            )}
          </div>
        )}
      </div>
    </Section>
  )
}

function EndpointForm({ baseUrl, model, hasKey }: { baseUrl: string; model: string; hasKey: boolean }) {
  const update = useUpdateAiSettings()
  const [url, setUrl] = useState(baseUrl)
  const [name, setName] = useState(model)
  const [key, setKey] = useState('')

  function save(event: FormEvent) {
    event.preventDefault()
    update.mutate(
      {
        embedder: 'openai',
        openai_base_url: url,
        openai_model: name,
        ...(key.trim() ? { openai_api_key: key.trim() } : {}),
      },
      { onSuccess: () => setKey('') },
    )
  }

  return (
    <form onSubmit={save} className="flex max-w-xl flex-col gap-4">
      <TextField
        label="Adresse"
        hint="z. B. https://api.openai.com/v1 oder http://192.168.1.10:11434/v1 (Ollama)"
        value={url}
        onChange={(e) => setUrl(e.target.value)}
      />
      <TextField label="Modell" hint="z. B. text-embedding-3-small oder nomic-embed-text" value={name} onChange={(e) => setName(e.target.value)} />
      <TextField
        label={hasKey ? 'Neuer API-Schlüssel (gespeichert)' : 'API-Schlüssel (optional)'}
        type="password"
        autoComplete="off"
        value={key}
        onChange={(e) => setKey(e.target.value)}
      />
      <FormError error={update.error} />
      <div className="flex gap-2">
        <Button type="submit" variant="accent" loading={update.isPending} disabled={!url.trim() || !name.trim()}>
          Prüfen & speichern
        </Button>
        {hasKey && (
          <Button type="button" variant="ghost" onClick={() => update.mutate({ openai_api_key: null })}>
            Schlüssel entfernen
          </Button>
        )}
      </div>
    </form>
  )
}

export function AiSettings() {
  const settings = useAiSettings()
  const status = useAiStatus()
  const update = useUpdateAiSettings()
  const [choice, setChoice] = useState<string | null>(null)
  if (!settings.data) return null
  const s = settings.data
  const selected = choice ?? s.embedder

  return (
    <div className="flex flex-col gap-6">
      {status.data && <StatusSection status={status.data} />}
      <Section
        title="Embeddings"
        description="Die lokale Variante läuft komplett auf diesem Server; das Modell (etwa 300 MB) wird beim ersten Bedarf geladen und geprüft. KI-Arbeit pausiert, solange jemand transkodiert schaut."
      >
        <div className="flex max-w-xl flex-col gap-4">
          <SelectField
            label="Quelle"
            value={selected}
            options={EMBEDDERS}
            onChange={(value) => {
              setChoice(value)
              if (value !== 'openai') update.mutate({ embedder: value as 'local' | 'off' })
            }}
          />
          {selected === 'local' && (
            <Checkbox
              label="Modell automatisch herunterladen"
              hint="Sonst über „Modell herunterladen“ im Status."
              checked={s.auto_download}
              onChange={(auto) => update.mutate({ auto_download: auto })}
            />
          )}
          {selected === 'openai' && (
            <EndpointForm baseUrl={s.openai_base_url ?? ''} model={s.openai_model ?? ''} hasKey={s.has_openai_api_key} />
          )}
          {selected !== 'openai' && <FormError error={update.error} />}
        </div>
      </Section>
    </div>
  )
}
