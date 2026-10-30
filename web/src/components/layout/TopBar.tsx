import { Link, useNavigate, useRouter, useRouterState } from '@tanstack/react-router'
import clsx from 'clsx'
import { ArrowLeft, Menu, Search, Settings } from 'lucide-react'
import { useCallback, useEffect, useId, useRef, useState } from 'react'
import { useItems } from '@/api/queries/items'
import { BlurImage } from '@/components/media/BlurImage'
import { useSession } from '@/lib/session'
import { AccountMenu } from './AccountMenu'
import { ActivityMenu } from './ActivityMenu'
import { Logo } from './Logo'
import { useDismiss } from './useDismiss'

const iconButton =
  'flex size-10 shrink-0 items-center justify-center rounded-full text-muted transition-colors hover:bg-surface-2 hover:text-fg'

/**
 * The top bar of the app, laid out like Plex Web: sidebar toggle, logo, back and
 * search on the left; activity, settings and the profile menu on the right.
 */
export function TopBar({ collapsed, onToggleSidebar }: { collapsed: boolean; onToggleSidebar?: () => void }) {
  const router = useRouter()
  const isAdmin = useSession()?.user.is_admin ?? false

  return (
    <header className="fixed inset-x-0 top-0 z-40 flex h-14 items-center gap-1 border-b border-line/60 bg-sidebar/95 px-2 backdrop-blur-xl md:gap-2 md:px-3">
      {onToggleSidebar && (
        <button
          type="button"
          onClick={onToggleSidebar}
          className={iconButton}
          aria-label={collapsed ? 'Seitenleiste ausklappen' : 'Seitenleiste einklappen'}
          title={collapsed ? 'Ausklappen' : 'Einklappen'}
        >
          <Menu className="size-5" aria-hidden="true" />
        </button>
      )}
      <Link to="/" aria-label="Zur Startseite" className="shrink-0 rounded-lg px-1">
        <Logo compact={collapsed} />
      </Link>
      <button
        type="button"
        onClick={() => router.history.back()}
        className={clsx(iconButton, 'hidden sm:flex')}
        aria-label="Vorherige Seite"
        title="Zurück"
      >
        <ArrowLeft className="size-5" aria-hidden="true" />
      </button>
      <SearchField />
      <div className="ml-auto flex shrink-0 items-center gap-0.5 md:gap-1">
        {isAdmin && <ActivityMenu />}
        <Link
          to="/settings"
          search={{ tab: isAdmin ? 'libraries' : 'account' }}
          className={iconButton}
          aria-label="Einstellungen"
          title="Einstellungen"
        >
          <Settings className="size-5" aria-hidden="true" />
        </Link>
        <AccountMenu />
      </div>
    </header>
  )
}

/**
 * Search across the library from anywhere: quick results while typing (movies and
 * series), Enter or "Alle Ergebnisse" opens the full search page.
 */
function SearchField() {
  const navigate = useNavigate()
  const onSearchPage = useRouterState({ select: (s) => s.location.pathname === '/search' })
  const [term, setTerm] = useState('')
  const [query, setQuery] = useState('')
  const [open, setOpen] = useState(false)
  const close = useCallback(() => setOpen(false), [])
  const root = useRef<HTMLFormElement>(null)
  useDismiss(root, open, close)
  const listId = useId()

  // Debounced: one request per pause in typing, not per key.
  useEffect(() => {
    const timer = setTimeout(() => setQuery(term.trim()), 200)
    return () => clearTimeout(timer)
  }, [term])

  const enabled = open && query.length >= 2
  const base = { search: query, sort: 'relevance' as const, limit: 4 }
  const movies = useItems({ ...base, kind: 'movie' }, enabled)
  const series = useItems({ ...base, kind: 'series' }, enabled)
  const results = [...(movies.data?.items ?? []), ...(series.data?.items ?? [])]

  const showAll = () => {
    const q = term.trim()
    if (!q) return
    setOpen(false)
    void navigate({ to: '/search', search: { q } })
  }

  return (
    <form
      ref={root}
      role="search"
      className="relative min-w-0 flex-1 md:max-w-md"
      onSubmit={(e) => {
        e.preventDefault()
        showAll()
      }}
    >
      <Search className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-subtle" aria-hidden="true" />
      <input
        type="search"
        value={term}
        onChange={(e) => {
          setTerm(e.target.value)
          setOpen(true)
        }}
        onFocus={() => setOpen(true)}
        placeholder="Suchen"
        aria-label="In der Mediathek finden"
        aria-controls={listId}
        aria-expanded={open && enabled}
        className="h-9 w-full rounded-full border border-transparent bg-surface-2 pr-3 pl-9 text-sm placeholder:text-subtle focus:border-line focus:bg-surface-3 focus:outline-none"
      />
      {open && enabled && !onSearchPage && (
        <div
          id={listId}
          className="absolute top-11 left-0 z-50 w-full min-w-72 overflow-hidden rounded-lg border border-line bg-surface-2 py-1 shadow-2xl"
          data-testid="quick-search"
        >
          {results.length === 0 && (movies.isFetching || series.isFetching) && (
            <p className="px-4 py-3 text-sm text-subtle">Suche…</p>
          )}
          {results.length === 0 && !movies.isFetching && !series.isFetching && (
            <p className="px-4 py-3 text-sm text-subtle">Keine Treffer für „{query}“.</p>
          )}
          <ul>
            {results.map((r) => (
              <li key={r.id}>
                <Link
                  to="/item/$id"
                  params={{ id: String(r.id) }}
                  onClick={() => setOpen(false)}
                  className="flex items-center gap-3 px-4 py-1.5 transition-colors hover:bg-surface-3"
                >
                  <span className="w-8 shrink-0 overflow-hidden rounded">
                    <BlurImage src={r.poster_url} blurhash={r.poster_blurhash} alt="" className="aspect-[2/3]" />
                  </span>
                  <span className="min-w-0">
                    <span className="block truncate text-sm font-semibold">{r.title}</span>
                    <span className="block text-xs text-subtle">
                      {[r.kind === 'series' ? 'Serie' : 'Film', r.year].filter(Boolean).join(' · ')}
                    </span>
                  </span>
                </Link>
              </li>
            ))}
          </ul>
          <button
            type="submit"
            className="mt-1 w-full border-t border-line/60 px-4 py-2 text-left text-sm font-semibold text-muted transition-colors hover:bg-surface-3 hover:text-fg"
          >
            Alle Ergebnisse für „{query}“
          </button>
        </div>
      )}
    </form>
  )
}
