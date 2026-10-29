import { Check, Layers, Plus } from 'lucide-react'
import { type FormEvent, useState } from 'react'
import { useAddToCollection, useCollections, useCreateCollection } from '@/api/queries/collections'
import { FormError } from '@/components/layout/AuthLayout'
import { Button } from '@/components/ui/Button'
import { Dialog } from '@/components/ui/Dialog'

/** Admin: put a movie or series into a manual collection (or a new one). */
export function AddToCollectionDialog({ item, onClose }: { item: { id: number; title: string }; onClose: () => void }) {
  const collections = useCollections()
  const add = useAddToCollection()
  const create = useCreateCollection()
  const [name, setName] = useState('')
  const [done, setDone] = useState<string | null>(null)
  const manual = collections.data?.filter((c) => c.kind === 'manual') ?? []

  function addTo(id: number, collectionName: string) {
    add.mutate({ id, itemIds: [item.id] }, { onSuccess: () => setDone(collectionName) })
  }

  function submit(event: FormEvent) {
    event.preventDefault()
    const trimmed = name.trim()
    if (!trimmed) return
    create.mutate({ name: trimmed }, { onSuccess: (created) => addTo(created.id, created.name) })
  }

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()} title="Zur Sammlung hinzufügen" description={`„${item.title}“`}>
      {done ? (
        <div className="space-y-4">
          <p className="flex items-center gap-2 text-sm" role="status">
            <Check className="size-4 text-accent" aria-hidden="true" />
            Zu „{done}“ hinzugefügt.
          </p>
          <div className="flex justify-end">
            <Button onClick={onClose}>Fertig</Button>
          </div>
        </div>
      ) : (
        <div className="space-y-4">
          <FormError error={add.error ?? create.error} />
          {manual.length > 0 && (
            <ul className="max-h-64 space-y-1 overflow-y-auto" aria-label="Eigene Sammlungen">
              {manual.map((c) => (
                <li key={c.id}>
                  <button
                    type="button"
                    disabled={add.isPending}
                    onClick={() => addTo(c.id, c.name)}
                    className="flex w-full items-center gap-3 rounded-lg px-3 py-2 text-left text-sm hover:bg-surface-2"
                  >
                    <Layers className="size-4 text-subtle" aria-hidden="true" />
                    <span className="min-w-0 flex-1 truncate font-semibold">{c.name}</span>
                    <span className="text-xs text-subtle">{c.item_count}</span>
                  </button>
                </li>
              ))}
            </ul>
          )}
          <form onSubmit={submit} className="flex gap-2">
            <input
              aria-label="Name der neuen Sammlung"
              placeholder="Neue Sammlung"
              value={name}
              onChange={(e) => setName(e.target.value)}
              className="h-10 min-w-0 flex-1 rounded-lg border border-line bg-surface-2 px-3 text-sm focus:border-accent focus:outline-none"
            />
            <Button type="submit" variant="accent" className="h-10 px-4" loading={create.isPending} disabled={!name.trim()}>
              <Plus className="size-4" aria-hidden="true" />
              Anlegen
            </Button>
          </form>
        </div>
      )}
    </Dialog>
  )
}
