import { Link } from '@tanstack/react-router'
import clsx from 'clsx'
import { Check, Film, SlidersHorizontal, Sparkles, Users } from 'lucide-react'
import { useState } from 'react'
import { type Recommendation, useOnboarding, useRecommendations, useSaveOnboarding } from '@/api/queries/ai'
import type { ItemCard } from '@/api/queries/items'
import { FormError } from '@/components/layout/AuthLayout'
import { BlurImage } from '@/components/media/BlurImage'
import { PosterCard, PosterSkeleton } from '@/components/media/PosterCard'
import { Button } from '@/components/ui/Button'
import { EmptyState } from '@/components/ui/EmptyState'
import { PageHeader } from '@/components/ui/PageHeader'
import { useSession } from '@/lib/session'

const GRID = { gridTemplateColumns: 'repeat(auto-fill, minmax(150px, 1fr))' }

/** "Für dich": personal recommendations with explanations, onboarding without history. */
export function ForYouPage() {
  const session = useSession()
  const recommendations = useRecommendations()
  const [editing, setEditing] = useState(false)
  const data = recommendations.data
  const onboarding = editing || data?.needs_onboarding === true

  return (
    <div className="pb-16">
      <PageHeader title="Für dich">
        <div className="flex flex-wrap gap-2">
          {data?.available && !onboarding && (
            <Button variant="ghost" onClick={() => setEditing(true)}>
              <SlidersHorizontal className="size-4" aria-hidden="true" />
              Geschmack anpassen
            </Button>
          )}
          <Link
            to="/group"
            className="inline-flex h-11 items-center gap-2 rounded-full bg-surface-2 px-5 text-sm font-bold text-fg hover:bg-surface-3"
          >
            <Users className="size-4" aria-hidden="true" />
            Was schauen wir heute?
          </Link>
        </div>
      </PageHeader>

      {recommendations.isPending && (
        <div className="grid gap-4 px-6 md:px-10" style={GRID}>
          {Array.from({ length: 12 }, (_, i) => (
            <PosterSkeleton key={i} />
          ))}
        </div>
      )}
      {data && !data.available && (
        <EmptyState icon={Sparkles} title="Empfehlungen sind noch nicht bereit">
          Flixnet lernt deine Bibliothek gerade kennen. Sobald die Titel analysiert sind, erscheinen hier Vorschläge.
          {session?.user.is_admin && (
            <>
              {' '}
              <Link to="/settings" search={{ tab: 'ai' }} className="font-semibold text-accent hover:text-accent-hover">
                KI-Einstellungen
              </Link>
            </>
          )}
        </EmptyState>
      )}
      {data?.available && onboarding && <Onboarding onDone={() => setEditing(false)} canCancel={editing} />}
      {data?.available && !onboarding && <RecommendationGrid items={data.items} />}
    </div>
  )
}

function RecommendationGrid({ items }: { items: Recommendation[] }) {
  if (items.length === 0) {
    return (
      <EmptyState icon={Film} title="Alles gesehen?">
        Für dich ist gerade nichts Neues dabei. Neue Titel in der Bibliothek bringen neue Vorschläge.
      </EmptyState>
    )
  }
  return (
    <div className="grid gap-x-4 gap-y-6 px-6 md:px-10" style={GRID} data-testid="recommendations">
      {items.map((rec) => (
        <div key={rec.item.id} data-testid="recommendation">
          <PosterCard item={rec.item} />
          <p className="mt-1 line-clamp-2 px-0.5 text-xs text-accent" data-testid="recommendation-reason">
            {rec.reason}
          </p>
        </div>
      ))}
    </div>
  )
}

/** Pick a few liked titles so recommendations can start without history. */
function Onboarding({ onDone, canCancel }: { onDone: () => void; canCancel: boolean }) {
  const onboarding = useOnboarding(true)
  const save = useSaveOnboarding()
  const [picked, setPicked] = useState<number[] | null>(null)
  const selected = picked ?? onboarding.data?.selected ?? []
  const toggle = (id: number) =>
    setPicked(selected.includes(id) ? selected.filter((x) => x !== id) : [...selected, id])

  return (
    <section className="px-6 md:px-10" aria-label="Geschmack">
      <div className="mb-6 flex flex-wrap items-end justify-between gap-4">
        <div>
          <h2 className="text-xl font-bold">Was gefällt dir?</h2>
          <p className="mt-1 text-sm text-muted">
            Wähle ein paar Filme oder Serien, die du magst – daraus entstehen deine ersten Empfehlungen.
          </p>
        </div>
        <div className="flex gap-2">
          {canCancel && (
            <Button variant="ghost" onClick={onDone}>
              Abbrechen
            </Button>
          )}
          <Button
            variant="accent"
            disabled={selected.length === 0}
            loading={save.isPending}
            onClick={() => save.mutate(selected, { onSuccess: onDone })}
          >
            {selected.length === 0 ? 'Titel auswählen' : `${selected.length} übernehmen`}
          </Button>
        </div>
      </div>
      <FormError error={save.error} />
      <div className="grid gap-4" style={GRID}>
        {onboarding.data?.items.map((item) => (
          <PickCard key={item.id} item={item} active={selected.includes(item.id)} onToggle={() => toggle(item.id)} />
        ))}
      </div>
    </section>
  )
}

function PickCard({ item, active, onToggle }: { item: ItemCard; active: boolean; onToggle: () => void }) {
  return (
    <button
      type="button"
      aria-pressed={active}
      onClick={onToggle}
      data-testid="onboarding-pick"
      className="group min-w-0 text-left focus-visible:outline-none"
    >
      <div
        className={clsx(
          'relative aspect-[2/3] overflow-hidden rounded-[var(--radius-card)] shadow-[var(--shadow-card)] transition',
          active ? 'ring-3 ring-accent' : 'group-hover:ring-2 group-hover:ring-line group-focus-visible:ring-2 group-focus-visible:ring-accent',
        )}
      >
        <BlurImage src={item.poster_url} blurhash={item.poster_blurhash} alt={item.title} className="absolute inset-0" />
        {!item.poster_url && (
          <div className="absolute inset-0 flex items-center justify-center p-3 text-center text-sm font-semibold text-muted">
            {item.title}
          </div>
        )}
        {active && (
          <span className="absolute top-2 right-2 flex size-7 items-center justify-center rounded-full bg-accent text-accent-fg shadow">
            <Check className="size-4" strokeWidth={3} aria-hidden="true" />
          </span>
        )}
      </div>
      <p className="mt-2 truncate px-0.5 text-sm font-semibold">{item.title}</p>
      <p className="truncate px-0.5 text-xs text-subtle">{item.year ?? ' '}</p>
    </button>
  )
}
