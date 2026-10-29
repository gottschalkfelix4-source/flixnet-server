import { Link } from '@tanstack/react-router'
import clsx from 'clsx'
import { CalendarDays, LayoutGrid, Radio, Search } from 'lucide-react'
import { useState } from 'react'
import {
  type Channel,
  type Program,
  formatClock,
  formatDay,
  formatSlot,
  progress,
  useChannels,
  useProgramSearch,
} from '@/api/queries/livetv'
import { ChannelLogo, GuideGrid } from '@/components/livetv/GuideGrid'
import { ProgramDialog } from '@/components/livetv/ProgramDialog'
import { EmptyState } from '@/components/ui/EmptyState'
import { PageHeader } from '@/components/ui/PageHeader'
import { useNow } from '@/lib/useNow'
import { useSession } from '@/lib/session'

type View = 'guide' | 'channels'

/** Live TV: the program guide, the channel list and program search. */
export function LiveTvPage() {
  const channels = useChannels()
  const now = useNow()
  const session = useSession()
  const [view, setView] = useState<View>('guide')
  const [query, setQuery] = useState('')
  const [selected, setSelected] = useState<Program | null>(null)
  const list = channels.data ?? []
  const byId = new Map(list.map((c) => [c.id, c]))

  return (
    <div className="pb-10">
      <PageHeader title="Live TV">
        {list.length > 0 && (
          <div className="flex flex-wrap items-center gap-3">
            <label className="relative">
              <Search className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-subtle" aria-hidden="true" />
              <input
                type="search"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder="Sendungen suchen"
                aria-label="Sendungen suchen"
                className="h-10 w-64 rounded-full border border-line bg-surface-2 pr-4 pl-9 text-sm focus:border-accent focus:outline-none"
              />
            </label>
            <div className="flex rounded-full bg-surface-2 p-1" role="tablist" aria-label="Ansicht">
              {(
                [
                  { id: 'guide', label: 'Programm', icon: CalendarDays },
                  { id: 'channels', label: 'Sender', icon: LayoutGrid },
                ] as const
              ).map((tab) => (
                <button
                  key={tab.id}
                  type="button"
                  role="tab"
                  aria-selected={view === tab.id}
                  onClick={() => setView(tab.id)}
                  className={clsx(
                    'inline-flex h-8 items-center gap-1.5 rounded-full px-4 text-sm font-semibold transition-colors',
                    view === tab.id ? 'bg-fg text-canvas' : 'text-muted hover:text-fg',
                  )}
                >
                  <tab.icon className="size-4" aria-hidden="true" />
                  {tab.label}
                </button>
              ))}
            </div>
          </div>
        )}
      </PageHeader>

      {channels.isPending ? (
        <p className="px-6 text-sm text-muted md:px-10">Wird geladen …</p>
      ) : list.length === 0 ? (
        <EmptyState icon={Radio} title="Noch keine Sender">
          {session?.user.is_admin ? (
            <>
              Richte unter{' '}
              <Link to="/settings" search={{ tab: 'livetv' }} className="font-semibold text-accent hover:text-accent-hover">
                Einstellungen → Live TV
              </Link>{' '}
              einen Tuner (M3U, Xtream Codes oder HDHomeRun) und eine Programmquelle ein.
            </>
          ) : (
            'Ein Administrator kann Tuner und Programmquellen in den Einstellungen einrichten.'
          )}
        </EmptyState>
      ) : (
        <div className="flex flex-col gap-6 px-6 md:px-10">
          {query.trim().length >= 2 && <SearchResults query={query} channels={byId} onProgram={setSelected} />}
          {view === 'guide' ? (
            <GuideGrid channels={list} now={now} onProgram={setSelected} />
          ) : (
            <ChannelList channels={list} now={now} onProgram={setSelected} />
          )}
        </div>
      )}

      {selected && (
        <ProgramDialog program={selected} channel={byId.get(selected.channel_id)} now={now} onClose={() => setSelected(null)} />
      )}
    </div>
  )
}

function SearchResults({
  query,
  channels,
  onProgram,
}: {
  query: string
  channels: Map<number, Channel>
  onProgram: (program: Program) => void
}) {
  const search = useProgramSearch(query)
  const results = search.data ?? []
  return (
    <section aria-label="Suchergebnisse" className="rounded-[var(--radius-card)] border border-line/70 bg-surface/70 p-4">
      {search.isPending ? (
        <p className="text-sm text-muted">Suche …</p>
      ) : results.length === 0 ? (
        <p className="text-sm text-muted">Keine kommenden Sendungen gefunden.</p>
      ) : (
        <ul className="divide-y divide-line/60" data-testid="program-results">
          {results.map((program) => (
            <li key={program.id}>
              <button
                type="button"
                onClick={() => onProgram(program)}
                className="flex w-full items-center gap-4 py-2.5 text-left hover:text-accent"
              >
                <span className="w-44 shrink-0 text-xs text-subtle">
                  {formatDay(program.start_ms)}
                  <br />
                  {formatSlot(program)}
                </span>
                <span className="min-w-0 flex-1">
                  <span className="block truncate text-sm font-bold">{program.title}</span>
                  <span className="block truncate text-xs text-muted">
                    {[channels.get(program.channel_id)?.name, program.subtitle].filter(Boolean).join(' · ')}
                  </span>
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </section>
  )
}

function ChannelList({
  channels,
  now,
  onProgram,
}: {
  channels: Channel[]
  now: number
  onProgram: (program: Program) => void
}) {
  return (
    <ul className="grid gap-3 sm:grid-cols-2 xl:grid-cols-3" data-testid="channel-list">
      {channels.map(({ now: current, next, ...channel }) => (
        <li key={channel.id} className="flex flex-col gap-3 rounded-[var(--radius-card)] border border-line/70 bg-surface/70 p-4">
          <Link
            to="/live/$channelId"
            params={{ channelId: String(channel.id) }}
            className="flex items-center gap-3 hover:text-accent"
          >
            <ChannelLogo channel={{ ...channel, now: current, next }} size="lg" />
            <span className="min-w-0">
              <span className="block text-xs font-bold text-subtle">
                {[channel.number, channel.group, channel.is_hd ? 'HD' : null].filter(Boolean).join(' · ')}
              </span>
              <span className="block truncate font-bold">{channel.name}</span>
            </span>
          </Link>
          {current ? (
            <button type="button" onClick={() => onProgram(current)} className="text-left">
              <span className="block truncate text-sm font-semibold">{current.title}</span>
              <span className="mt-1.5 block h-1 overflow-hidden rounded-full bg-surface-3">
                <span className="block h-full bg-accent" style={{ width: `${progress(current, now) * 100}%` }} />
              </span>
            </button>
          ) : (
            <p className="text-sm text-subtle">Kein Programm verfügbar</p>
          )}
          {next && (
            <button type="button" onClick={() => onProgram(next)} className="truncate text-left text-xs text-muted hover:text-fg">
              Danach {formatClock(next.start_ms)}: {next.title}
            </button>
          )}
        </li>
      ))}
    </ul>
  )
}
