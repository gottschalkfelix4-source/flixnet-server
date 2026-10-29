import { Search } from 'lucide-react'
import { type FormEvent, useState } from 'react'
import { useIdentify, useIdentifyCandidates } from '@/api/queries/items'
import { FormError } from '@/components/layout/AuthLayout'
import { Button } from '@/components/ui/Button'
import { Dialog } from '@/components/ui/Dialog'

/** Admin: search TMDB and pin the item to the right entry. */
export function IdentifyDialog({ itemId, title, onClose }: { itemId: number; title: string; onClose: () => void }) {
  const [input, setInput] = useState(title)
  const [query, setQuery] = useState(title)
  const candidates = useIdentifyCandidates(itemId, query, true)
  const identify = useIdentify()

  function submit(event: FormEvent) {
    event.preventDefault()
    setQuery(input.trim())
  }

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()} title="Identifizieren" description="Wähle den passenden Eintrag aus TMDB.">
      <form onSubmit={submit} className="mb-4 flex gap-2">
        <input
          aria-label="Suchbegriff"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          className="h-10 min-w-0 flex-1 rounded-lg border border-line bg-surface-2 px-3 text-sm focus:border-accent focus:outline-none"
        />
        <Button type="submit" variant="ghost" className="h-10 px-3" aria-label="Suchen">
          <Search className="size-4" />
        </Button>
      </form>
      <FormError error={candidates.error ?? identify.error} />
      <ul className="max-h-96 space-y-2 overflow-y-auto">
        {candidates.isPending && <li className="text-sm text-muted">Suche läuft…</li>}
        {candidates.data?.length === 0 && <li className="text-sm text-muted">Nichts gefunden.</li>}
        {candidates.data?.map((c) => (
          <li key={c.id}>
            <button
              type="button"
              disabled={identify.isPending}
              onClick={() => identify.mutate({ id: itemId, tmdbId: c.id }, { onSuccess: onClose })}
              className="flex w-full gap-3 rounded-lg p-2 text-left hover:bg-surface-2"
            >
              <span className="h-20 w-14 shrink-0 overflow-hidden rounded bg-surface-3">
                {c.poster_url && <img src={c.poster_url.replace('/original/', '/w154/')} alt="" className="size-full object-cover" />}
              </span>
              <span className="min-w-0">
                <span className="block text-sm font-bold">
                  {c.title} {c.year && <span className="font-normal text-muted">({c.year})</span>}
                </span>
                {c.original_title !== c.title && <span className="block text-xs text-subtle">{c.original_title}</span>}
                <span className="line-clamp-2 text-xs text-muted">{c.overview}</span>
                <span className="text-[11px] text-subtle">Übereinstimmung {Math.round(c.score * 100)} %</span>
              </span>
            </button>
          </li>
        ))}
      </ul>
    </Dialog>
  )
}
