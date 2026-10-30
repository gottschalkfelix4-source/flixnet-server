import { Database, FileCog } from 'lucide-react'
import { type FormEvent, useState } from 'react'
import { useBackups, useCreateBackup, useUpdateBackupSettings } from '@/api/queries/ops'
import { FormError } from '@/components/layout/AuthLayout'
import { Button } from '@/components/ui/Button'
import { Section } from '@/components/ui/Section'
import { TextField } from '@/components/ui/TextField'
import { formatBytes } from '@/lib/bytes'
import { formatDateTime } from '@/lib/format'

/** Database backups: folder, retention, "Jetzt sichern" and how to restore. */
export function BackupSettings() {
  const backups = useBackups()
  const create = useCreateBackup()
  const update = useUpdateBackupSettings()
  const [directory, setDirectory] = useState<string | null>(null)
  const [keep, setKeep] = useState<string | null>(null)

  if (!backups.data) return null
  const b = backups.data

  function save(event: FormEvent) {
    event.preventDefault()
    update.mutate(
      {
        directory: directory ?? undefined,
        keep: keep !== null ? Number(keep) : undefined,
      },
      {
        onSuccess: () => {
          setDirectory(null)
          setKeep(null)
        },
      },
    )
  }

  return (
    <div className="flex flex-col gap-6">
      <Section
        title="Sicherungen"
        description="Flixnet sichert die Datenbank (Nutzer, Gesehen-Status, Einstellungen) und die Konfigurationsdatei täglich. Medien und Bild-Cache sind nicht enthalten."
        action={
          <Button variant="accent" className="h-9 px-4" loading={create.isPending} onClick={() => create.mutate()}>
            <Database className="size-4" aria-hidden="true" />
            Jetzt sichern
          </Button>
        }
      >
        <FormError error={create.error} />
        {create.isSuccess && (
          <p role="status" className="mb-3 text-sm text-emerald-400">
            Sicherung {create.data.name} erstellt.
          </p>
        )}
        {b.files.length === 0 && <p className="text-sm text-muted">Noch keine Sicherung vorhanden.</p>}
        <ul className="divide-y divide-line/60" data-testid="backup-list">
          {b.files.map((f) => (
            <li key={f.name} className="flex flex-wrap items-center gap-3 py-3 text-sm">
              <Database className="size-4 text-muted" aria-hidden="true" />
              <span className="min-w-0 flex-1 truncate font-mono text-xs">{f.name}</span>
              {f.has_config && (
                <span className="flex items-center gap-1 text-xs text-subtle">
                  <FileCog className="size-3.5" aria-hidden="true" />
                  mit Konfiguration
                </span>
              )}
              <span className="tabular-nums text-muted">{formatBytes(f.size_bytes)}</span>
              <span className="text-xs text-subtle">{formatDateTime(f.created_at)}</span>
            </li>
          ))}
        </ul>
      </Section>

      <Section title="Ort und Aufbewahrung" description="Am besten ein Ordner außerhalb von /config, z. B. auf dem Array.">
        <form onSubmit={save} className="grid gap-4 md:grid-cols-[1fr_10rem_auto] md:items-end">
          <TextField
            label="Ordner"
            hint={b.is_default_directory ? 'Standard: backups im Konfigurationsordner. Leer lassen für den Standard.' : 'Leer lassen für den Standard.'}
            value={directory ?? (b.is_default_directory ? '' : b.directory)}
            placeholder={b.directory}
            onChange={(e) => setDirectory(e.target.value)}
          />
          <TextField
            label="Anzahl behalten"
            type="number"
            min={1}
            max={365}
            value={keep ?? String(b.keep)}
            onChange={(e) => setKeep(e.target.value)}
          />
          <Button type="submit" loading={update.isPending} disabled={directory === null && keep === null}>
            Speichern
          </Button>
        </form>
        <div className="mt-3">
          <FormError error={update.error} />
        </div>
      </Section>

      <Section title="Wiederherstellen" description="Eine Sicherung wird bei gestopptem Server über die Kommandozeile zurückgespielt.">
        <ol className="list-decimal space-y-2 pl-5 text-sm text-muted">
          <li>Flixnet stoppen (Unraid: Container anhalten).</li>
          <li>
            Wiederherstellen, optional mit <code className="text-fg">--with-config</code> für die Konfigurationsdatei:
            <pre className="mt-2 overflow-x-auto rounded-lg bg-canvas/80 p-3 font-mono text-xs text-fg" data-testid="restore-command">
              {b.restore_command}
            </pre>
            Im Docker-Container:{' '}
            <code className="text-fg">docker run --rm -v /mnt/cache/appdata/flixnet:/config ghcr.io/gottschalkfelix4-source/flixnet-server flixnet-server restore /config/backups/&lt;datei&gt;.db</code>
          </li>
          <li>Flixnet wieder starten. Die vorherige Datenbank bleibt als flixnet.db.before-restore-… erhalten.</li>
        </ol>
      </Section>
    </div>
  )
}
