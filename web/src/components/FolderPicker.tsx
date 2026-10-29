import { ArrowUp, Folder, HardDrive } from 'lucide-react'
import { useState } from 'react'
import { useDirectories } from '@/api/queries/libraries'
import { Button } from '@/components/ui/Button'

/** Browses server folders; calls `onPick` with the chosen absolute path. */
export function FolderPicker({ onPick }: { onPick: (path: string) => void }) {
  const [path, setPath] = useState<string | null>(null)
  const listing = useDirectories(path)

  return (
    <div className="rounded-lg border border-line bg-surface-2">
      <div className="flex items-center gap-2 border-b border-line px-3 py-2">
        <button
          type="button"
          onClick={() => setPath(listing.data?.parent ?? null)}
          disabled={path === null}
          aria-label="Übergeordneter Ordner"
          className="rounded p-1 text-muted hover:bg-surface-3 hover:text-fg disabled:opacity-30"
        >
          <ArrowUp className="size-4" />
        </button>
        <code className="min-w-0 flex-1 truncate text-xs text-muted" data-testid="picker-path">
          {path ?? 'Laufwerke'}
        </code>
        {path && (
          <Button type="button" variant="accent" className="h-8 px-3 text-xs" onClick={() => onPick(path)}>
            Diesen Ordner wählen
          </Button>
        )}
      </div>
      <ul className="max-h-56 overflow-y-auto py-1" aria-label="Ordner">
        {listing.data?.directories.length === 0 && (
          <li className="px-3 py-2 text-xs text-subtle">Keine Unterordner.</li>
        )}
        {listing.data?.directories.map((dir) => (
          <li key={dir.path}>
            <button
              type="button"
              onClick={() => setPath(dir.path)}
              className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm hover:bg-surface-3"
            >
              {path === null ? (
                <HardDrive className="size-4 text-muted" aria-hidden="true" />
              ) : (
                <Folder className="size-4 text-accent" aria-hidden="true" />
              )}
              <span className="truncate">{dir.name}</span>
            </button>
          </li>
        ))}
      </ul>
    </div>
  )
}
