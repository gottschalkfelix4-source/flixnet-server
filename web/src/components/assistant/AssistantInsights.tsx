import { History, Lightbulb } from 'lucide-react'
import { useState } from 'react'
import { useExplanation, useRecap } from '@/api/queries/assistant'
import type { ItemDetail } from '@/api/queries/items'
import { Button } from '@/components/ui/Button'
import { Dialog } from '@/components/ui/Dialog'

/** Under the item header: why it fits the viewer (movies, series) and a spoiler-free
 *  "Was bisher geschah" (episodes, and movies already started). */
export function AssistantInsights({ item }: { item: ItemDetail }) {
  const explainable = item.kind === 'movie' || item.kind === 'series'
  const recappable = item.kind === 'episode' || (item.kind === 'movie' && item.position_ms > 0)
  const explanation = useExplanation(item.id, explainable)
  const text = explanation.data?.text
  if (!text && !recappable) return null
  return (
    <div className="flex flex-col gap-3 px-6 pb-6 md:px-10">
      {text && (
        <p className="flex max-w-3xl items-start gap-2 text-sm text-fg/85" data-testid="item-explanation">
          <Lightbulb className="mt-0.5 size-4 shrink-0 text-amber-300" aria-hidden="true" />
          {text}
        </p>
      )}
      {recappable && <RecapButton item={item} />}
    </div>
  )
}

function RecapButton({ item }: { item: ItemDetail }) {
  const recap = useRecap()
  const [open, setOpen] = useState(false)
  function show() {
    setOpen(true)
    recap.mutate({ id: item.id })
  }
  return (
    <>
      <div>
        <Button variant="ghost" className="h-9 px-4" onClick={show}>
          <History className="size-4" aria-hidden="true" />
          Was bisher geschah
        </Button>
      </div>
      <Dialog
        open={open}
        onOpenChange={setOpen}
        title="Was bisher geschah"
        description="Ohne Spoiler: nur was vor deiner aktuellen Stelle passiert ist."
      >
        {recap.isPending && <p className="text-sm text-muted">Zusammenfassung wird erstellt…</p>}
        {recap.error && (
          <p role="alert" className="text-sm text-red-300">
            {recap.error.message}
          </p>
        )}
        {recap.data && (
          <div data-testid="recap" className="flex flex-col gap-3">
            {recap.data.text ? (
              <p className="text-sm leading-relaxed whitespace-pre-line">{recap.data.text}</p>
            ) : (
              <p className="text-sm text-muted">Bis zu dieser Stelle gibt es noch nichts zusammenzufassen.</p>
            )}
            {recap.data.covers.length > 0 && (
              <p className="text-xs text-subtle">Grundlage: {recap.data.covers.join(' · ')}</p>
            )}
          </div>
        )}
      </Dialog>
    </>
  )
}
