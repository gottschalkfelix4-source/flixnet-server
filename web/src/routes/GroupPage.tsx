import { Link } from '@tanstack/react-router'
import clsx from 'clsx'
import { ChevronLeft, RotateCcw, ShieldCheck, ThumbsDown, ThumbsUp, Trophy, Users } from 'lucide-react'
import { type ReactNode, useMemo, useState } from 'react'
import { type GroupSuggestion, useGroupRecommendations } from '@/api/queries/ai'
import { useProfiles } from '@/api/queries/users'
import { FormError } from '@/components/layout/AuthLayout'
import { PosterCard } from '@/components/media/PosterCard'
import { Button } from '@/components/ui/Button'
import { EmptyState } from '@/components/ui/EmptyState'
import { PageHeader } from '@/components/ui/PageHeader'
import { Checkbox } from '@/components/ui/Section'

interface Votes {
  up: number
  down: number
}

/**
 * "Was schauen wir heute?": pick who is watching, get suggestions that suit everyone
 * (strictest parental controls of the group), then vote on this device.
 */
export function GroupPage() {
  const profiles = useProfiles()
  const suggest = useGroupRecommendations()
  const [chosen, setChosen] = useState<number[]>([])
  const [excludeWatched, setExcludeWatched] = useState(true)
  const [votes, setVotes] = useState<Record<number, Votes>>({})

  const me = profiles.data?.find((p) => p.is_current)
  const others = profiles.data?.filter((p) => !p.is_current) ?? []
  const toggle = (id: number) => setChosen((c) => (c.includes(id) ? c.filter((x) => x !== id) : [...c, id]))

  const run = () => {
    setVotes({})
    suggest.mutate({ user_ids: chosen, exclude_watched: excludeWatched, limit: 20 })
  }
  const vote = (id: number, kind: keyof Votes) =>
    setVotes((v) => ({ ...v, [id]: { up: 0, down: 0, ...v[id], [kind]: (v[id]?.[kind] ?? 0) + 1 } }))

  const result = suggest.data
  // Highest balance first; untouched suggestions keep their order.
  const ranked = useMemo(() => {
    if (!result) return []
    const balance = (s: GroupSuggestion) => (votes[s.item.id]?.up ?? 0) - (votes[s.item.id]?.down ?? 0)
    return result.items
      .map((s, index) => ({ s, index }))
      .sort((a, b) => balance(b.s) - balance(a.s) || a.index - b.index)
      .map(({ s }) => s)
  }, [result, votes])
  const anyVote = Object.values(votes).some((v) => v.up + v.down > 0)
  const winner = anyVote && ranked[0] && (votes[ranked[0].item.id]?.up ?? 0) > 0 ? ranked[0].item.id : null
  const names = new Map(result?.members.map((m) => [m.id, m.name]))

  return (
    <div className="pb-16">
      <PageHeader title="Was schauen wir heute?">
        <Link to="/for-you" className="inline-flex items-center gap-1 text-sm font-semibold text-muted hover:text-fg">
          <ChevronLeft className="size-4" aria-hidden="true" />
          Für dich
        </Link>
      </PageHeader>

      <section className="mx-6 mb-8 rounded-[var(--radius-card)] border border-line/70 bg-surface/70 p-5 md:mx-10" aria-label="Wer schaut mit?">
        <h2 className="mb-1 text-lg font-bold">Wer schaut mit?</h2>
        <p className="mb-4 text-sm text-muted">
          Die Vorschläge passen zu allen und halten den strengsten Jugendschutz der Gruppe ein.
        </p>
        <div className="mb-4 flex flex-wrap gap-2" role="group" aria-label="Profile">
          {me && (
            <span className="inline-flex h-10 items-center rounded-full bg-accent px-4 text-sm font-bold text-accent-fg">
              {me.name} (du)
            </span>
          )}
          {others.map((p) => (
            <button
              key={p.id}
              type="button"
              aria-pressed={chosen.includes(p.id)}
              onClick={() => toggle(p.id)}
              className={clsx(
                'inline-flex h-10 items-center rounded-full px-4 text-sm font-bold transition-colors',
                chosen.includes(p.id) ? 'bg-fg text-canvas' : 'bg-surface-2 text-muted hover:text-fg',
              )}
            >
              {p.name}
            </button>
          ))}
        </div>
        <div className="flex flex-wrap items-center justify-between gap-4">
          <Checkbox label="Schon Gesehenes ausblenden" checked={excludeWatched} onChange={setExcludeWatched} />
          <Button variant="accent" onClick={run} loading={suggest.isPending}>
            <Users className="size-4" aria-hidden="true" />
            Vorschläge holen
          </Button>
        </div>
        <div className="mt-3">
          <FormError error={suggest.error} />
        </div>
      </section>

      {result && !result.available && (
        <EmptyState icon={Users} title="Noch keine Vorschläge möglich">
          Die Bibliothek wird gerade analysiert. Versuch es gleich noch einmal.
        </EmptyState>
      )}
      {result?.available && (
        <>
          <div className="mb-4 flex flex-wrap items-center gap-x-6 gap-y-2 px-6 text-sm text-muted md:px-10">
            {result.max_age_rating != null && (
              <span className="inline-flex items-center gap-1.5" data-testid="group-age-limit">
                <ShieldCheck className="size-4 text-emerald-400" aria-hidden="true" />
                Freigegeben ab {result.max_age_rating} Jahren
              </span>
            )}
            {result.members
              .filter((m) => !m.has_history)
              .map((m) => (
                <span key={m.id}>{m.name} hat noch nichts gesehen und zählt nur beim Jugendschutz.</span>
              ))}
            {anyVote && (
              <button type="button" onClick={() => setVotes({})} className="inline-flex items-center gap-1 font-semibold hover:text-fg">
                <RotateCcw className="size-3.5" aria-hidden="true" />
                Stimmen zurücksetzen
              </button>
            )}
          </div>
          {ranked.length === 0 ? (
            <EmptyState icon={Users} title="Nichts gefunden">
              Für diese Gruppe gibt es gerade keinen passenden Titel.
            </EmptyState>
          ) : (
            <div
              className="grid gap-x-4 gap-y-6 px-6 md:px-10"
              style={{ gridTemplateColumns: 'repeat(auto-fill, minmax(160px, 1fr))' }}
              data-testid="group-suggestions"
            >
              {ranked.map((s) => (
                <div key={s.item.id} data-testid="group-suggestion" className="relative">
                  {winner === s.item.id && (
                    <span className="absolute -top-2 left-1/2 z-10 inline-flex -translate-x-1/2 items-center gap-1 rounded-full bg-accent px-2.5 py-0.5 text-xs font-bold text-accent-fg shadow">
                      <Trophy className="size-3.5" aria-hidden="true" />
                      Favorit
                    </span>
                  )}
                  <PosterCard item={s.item} />
                  <p className="mt-1 line-clamp-2 px-0.5 text-xs text-accent" title={s.matches.map((m) => names.get(m)).join(', ')}>
                    {s.reason}
                  </p>
                  <div className="mt-2 flex gap-2">
                    <VoteButton label={`Daumen hoch für ${s.item.title}`} count={votes[s.item.id]?.up ?? 0} onClick={() => vote(s.item.id, 'up')}>
                      <ThumbsUp className="size-4" aria-hidden="true" />
                    </VoteButton>
                    <VoteButton label={`Daumen runter für ${s.item.title}`} count={votes[s.item.id]?.down ?? 0} onClick={() => vote(s.item.id, 'down')}>
                      <ThumbsDown className="size-4" aria-hidden="true" />
                    </VoteButton>
                  </div>
                </div>
              ))}
            </div>
          )}
        </>
      )}
    </div>
  )
}

function VoteButton({
  label,
  count,
  onClick,
  children,
}: {
  label: string
  count: number
  onClick: () => void
  children: ReactNode
}) {
  return (
    <button
      type="button"
      aria-label={label}
      onClick={onClick}
      className={clsx(
        'inline-flex h-8 flex-1 items-center justify-center gap-1.5 rounded-full text-sm font-bold transition-colors',
        count > 0 ? 'bg-surface-3 text-fg' : 'bg-surface-2 text-muted hover:text-fg',
      )}
    >
      {children}
      <span>{count}</span>
    </button>
  )
}
