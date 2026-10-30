import { BookAudio, Clapperboard, Film, FolderPlus, Images, Music, Plus, RefreshCw, Trash2, X } from 'lucide-react'
import { type FormEvent, useState } from 'react'
import type { Schemas } from '@/api/client'
import {
  useCreateLibrary,
  useDeleteLibrary,
  useLibraries,
  useScanLibrary,
  useUpdateLibrary,
} from '@/api/queries/libraries'
import { FolderPicker } from '@/components/FolderPicker'
import { FormError } from '@/components/layout/AuthLayout'
import { Button } from '@/components/ui/Button'
import { Dialog } from '@/components/ui/Dialog'
import { Checkbox, Section, SelectField } from '@/components/ui/Section'
import { TextField } from '@/components/ui/TextField'
import { relativeTime } from '@/lib/format'

type Library = Schemas['LibraryDto']
type LibraryKind = Schemas['LibraryKind']

function countLabel(library: Library): string {
  const c = library.counts
  if (library.kind === 'photos' || library.kind === 'audiobooks') {
    return library.kind === 'photos' ? 'Fotos' : 'Hörbücher'
  }
  if (library.kind === 'shows') {
    return `${c.series} ${c.series === 1 ? 'Serie' : 'Serien'} · ${c.episodes} ${c.episodes === 1 ? 'Folge' : 'Folgen'}`
  }
  if (library.kind === 'music') {
    return `${c.albums} ${c.albums === 1 ? 'Album' : 'Alben'} · ${c.tracks} ${c.tracks === 1 ? 'Titel' : 'Titel'}`
  }
  return `${c.movies} ${c.movies === 1 ? 'Film' : 'Filme'}`
}

export function LibrariesSettings() {
  const libraries = useLibraries()
  const scan = useScanLibrary()
  const [editing, setEditing] = useState<Library | 'new' | null>(null)
  const [now] = useState(() => Date.now())

  return (
    <Section
      title="Bibliotheken"
      description="Ordner mit Filmen, Serien, Musik, Fotos oder Hörbüchern. Neue Dateien werden beim Scannen erkannt."
      action={
        <Button variant="accent" className="h-10" onClick={() => setEditing('new')}>
          <Plus className="size-4" aria-hidden="true" />
          Bibliothek hinzufügen
        </Button>
      }
    >
      {libraries.data?.length === 0 && (
        <p className="text-sm text-muted">Noch keine Bibliotheken. Füge deinen ersten Medienordner hinzu.</p>
      )}
      <ul className="divide-y divide-line/60" data-testid="library-list">
        {libraries.data?.map((library) => {
          const Icon =
            library.kind === 'shows'
              ? Clapperboard
              : library.kind === 'music'
                ? Music
                : library.kind === 'photos'
                  ? Images
                  : library.kind === 'audiobooks'
                    ? BookAudio
                    : Film
          return (
            <li key={library.id} className="flex flex-wrap items-center gap-4 py-3">
              <Icon className="size-5 shrink-0 text-accent" aria-hidden="true" />
              <button type="button" onClick={() => setEditing(library)} className="min-w-0 flex-1 text-left">
                <p className="text-sm font-bold">{library.name}</p>
                <p className="text-xs text-subtle" data-testid="library-counts">
                  {countLabel(library)} · {library.counts.files} Dateien
                  {library.counts.unprobed > 0 && ` · ${library.counts.unprobed} werden analysiert`}
                  {library.last_scan_at ? ` · gescannt ${relativeTime(library.last_scan_at, now)}` : ' · noch nicht gescannt'}
                </p>
              </button>
              <Button
                variant="ghost"
                className="h-9 px-4"
                onClick={() => scan.mutate(library.id)}
                loading={scan.isPending && scan.variables === library.id}
              >
                <RefreshCw className="size-4" aria-hidden="true" />
                Scannen
              </Button>
            </li>
          )
        })}
      </ul>
      {editing && (
        <LibraryDialog
          key={editing === 'new' ? 'new' : editing.id}
          library={editing === 'new' ? null : editing}
          onClose={() => setEditing(null)}
        />
      )}
    </Section>
  )
}

function LibraryDialog({ library, onClose }: { library: Library | null; onClose: () => void }) {
  const create = useCreateLibrary()
  const update = useUpdateLibrary()
  const remove = useDeleteLibrary()
  const [name, setName] = useState(library?.name ?? '')
  const [kind, setKind] = useState<LibraryKind>((library?.kind as LibraryKind) ?? 'movies')
  const [paths, setPaths] = useState<string[]>(library?.paths ?? [])
  const [introDetection, setIntroDetection] = useState(library?.intro_detection ?? true)
  const [manualPath, setManualPath] = useState('')
  const [browsing, setBrowsing] = useState(false)
  const [confirmDelete, setConfirmDelete] = useState(false)
  const error = create.error ?? update.error ?? remove.error

  function addPath(path: string) {
    const trimmed = path.trim()
    if (trimmed && !paths.includes(trimmed)) setPaths([...paths, trimmed])
    setManualPath('')
    setBrowsing(false)
  }

  function submit(event: FormEvent) {
    event.preventDefault()
    const allPaths = manualPath.trim() ? [...paths, manualPath.trim()] : paths
    if (library) {
      update.mutate(
        { id: library.id, name: name.trim(), paths: allPaths, intro_detection: introDetection },
        { onSuccess: onClose },
      )
    } else {
      create.mutate(
        { name: name.trim(), kind, paths: allPaths, intro_detection: introDetection },
        { onSuccess: onClose },
      )
    }
  }

  return (
    <Dialog
      open
      onOpenChange={(open) => !open && onClose()}
      title={library ? `${library.name} bearbeiten` : 'Bibliothek hinzufügen'}
    >
      <form onSubmit={submit} className="flex flex-col gap-4">
        <TextField label="Name" value={name} maxLength={64} onChange={(e) => setName(e.target.value)} />
        {!library && (
          <SelectField
            label="Inhalt"
            value={kind}
            onChange={(v) => setKind(v as LibraryKind)}
            options={[
              { value: 'movies', label: 'Filme' },
              { value: 'shows', label: 'Serien' },
              { value: 'music', label: 'Musik' },
              { value: 'photos', label: 'Fotos' },
              { value: 'audiobooks', label: 'Hörbücher' },
            ]}
          />
        )}
        <div className="flex flex-col gap-2">
          <span className="text-sm font-semibold text-muted">Ordner</span>
          {paths.map((path) => (
            <div key={path} className="flex items-center gap-2 rounded-lg bg-surface-2 px-3 py-2">
              <code className="min-w-0 flex-1 truncate text-xs">{path}</code>
              <button
                type="button"
                aria-label={`${path} entfernen`}
                onClick={() => setPaths(paths.filter((p) => p !== path))}
                className="text-muted hover:text-fg"
              >
                <X className="size-4" />
              </button>
            </div>
          ))}
          <div className="flex gap-2">
            <input
              aria-label="Ordnerpfad"
              placeholder="/mnt/user/Filme"
              value={manualPath}
              onChange={(e) => setManualPath(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && manualPath.trim()) {
                  e.preventDefault()
                  addPath(manualPath)
                }
              }}
              className="h-10 min-w-0 flex-1 rounded-lg border border-line bg-surface-2 px-3 text-sm focus:border-accent focus:outline-none"
            />
            <Button type="button" variant="ghost" className="h-10 px-3" onClick={() => setBrowsing(!browsing)}>
              <FolderPlus className="size-4" aria-hidden="true" />
              Durchsuchen
            </Button>
          </div>
          {browsing && <FolderPicker onPick={addPath} />}
        </div>
        {(library?.kind ?? kind) === 'shows' && (
          <Checkbox
            label="Intro-Erkennung"
            hint="Intros und Abspänne der Folgen einer Staffel erkennen, damit sie sich überspringen lassen."
            checked={introDetection}
            onChange={setIntroDetection}
          />
        )}
        <FormError error={error} />
        <div className="flex flex-wrap items-center justify-between gap-3 pt-2">
          {library ? (
            confirmDelete ? (
              <Button
                type="button"
                className="bg-red-500 text-white hover:bg-red-400"
                loading={remove.isPending}
                onClick={() => remove.mutate(library.id, { onSuccess: onClose })}
              >
                Bibliothek entfernen
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
          <Button
            type="submit"
            variant="accent"
            loading={create.isPending || update.isPending}
            disabled={!name.trim() || (paths.length === 0 && !manualPath.trim())}
          >
            Speichern
          </Button>
        </div>
        {confirmDelete && (
          <p className="text-xs text-subtle">Die Dateien auf der Festplatte bleiben erhalten.</p>
        )}
      </form>
    </Dialog>
  )
}
