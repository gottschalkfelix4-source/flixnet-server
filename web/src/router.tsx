import type { QueryClient } from '@tanstack/react-query'
import {
  Outlet,
  createRootRouteWithContext,
  createRoute,
  createRouter,
  redirect,
} from '@tanstack/react-router'
import { ApiError, setUnauthorizedHandler } from '@/api/client'
import { currentUserQuery, setupStatusQuery } from '@/api/queries/auth'
import { AppShell } from '@/components/layout/AppShell'
import { itemQuery } from '@/api/queries/items'
import { getSession } from '@/lib/session'
import { type BrowseSearch, BrowsePage } from '@/routes/BrowsePage'
import { CollectionPage } from '@/routes/CollectionPage'
import { CollectionsPage } from '@/routes/CollectionsPage'
import { ForYouPage } from '@/routes/ForYouPage'
import { GroupPage } from '@/routes/GroupPage'
import { HomePage } from '@/routes/HomePage'
import { ItemPage } from '@/routes/ItemPage'
import { LivePlayerPage } from '@/routes/LivePlayerPage'
import { LiveTvPage } from '@/routes/LiveTvPage'
import { PersonPage } from '@/routes/PersonPage'
import { PlaylistPage } from '@/routes/PlaylistPage'
import { PlaylistsPage } from '@/routes/PlaylistsPage'
import { PlayerPage } from '@/routes/PlayerPage'
import { SearchPage } from '@/routes/SearchPage'
import { InvitePage } from '@/routes/InvitePage'
import { LoginPage } from '@/routes/LoginPage'
import { NotFoundPage } from '@/routes/NotFoundPage'
import { PlaceholderPage } from '@/routes/PlaceholderPage'
import { ProfilesPage } from '@/routes/ProfilesPage'
import { SetupPage } from '@/routes/SetupPage'
import { SettingsPage } from '@/routes/settings/SettingsPage'
import { SETTINGS_TABS, type SettingsTab } from '@/routes/settings/tabs'

interface RouterContext {
  queryClient: QueryClient
}

const rootRoute = createRootRouteWithContext<RouterContext>()({
  component: Outlet,
  notFoundComponent: NotFoundPage,
})

/** First-run wizard; only reachable while no account exists. */
const setupRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/setup',
  component: SetupPage,
  beforeLoad: async ({ context }) => {
    const status = await context.queryClient.ensureQueryData(setupStatusQuery)
    if (!status.setup_required) throw redirect({ to: getSession() ? '/' : '/login' })
  },
})

const loginRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/login',
  component: LoginPage,
  validateSearch: (search: Record<string, unknown>): { redirect?: string } =>
    typeof search.redirect === 'string' ? { redirect: search.redirect } : {},
  beforeLoad: async ({ context }) => {
    const status = await context.queryClient.ensureQueryData(setupStatusQuery)
    if (status.setup_required) throw redirect({ to: '/setup' })
    if (getSession()) throw redirect({ to: '/' })
  },
})

/** Everything behind sign-in shares the sidebar shell. */
const appRoute = createRoute({
  getParentRoute: () => rootRoute,
  id: 'app',
  component: AppShell,
  beforeLoad: requireSession,
})

/** Guard for signed-in areas: sends to setup or sign-in, verifies the token once. */
async function requireSession({
  context,
  location,
}: {
  context: RouterContext
  location: { href: string }
}) {
  const toLogin = () =>
    redirect({ to: '/login', search: location.href === '/' ? {} : { redirect: location.href } })

  if (!getSession()) {
    const status = await context.queryClient.ensureQueryData(setupStatusQuery)
    if (status.setup_required) throw redirect({ to: '/setup' })
    throw toLogin()
  }
  // Verify the stored token once (cached); a revoked token clears the session.
  try {
    await context.queryClient.ensureQueryData(currentUserQuery)
  } catch (error) {
    if (error instanceof ApiError && error.status === 401) throw toLogin()
    throw error
  }
}

/** Full-screen "Who's watching?" picker (signed in, but without the sidebar). */
const profilesRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/profiles',
  component: ProfilesPage,
  beforeLoad: requireSession,
})

/** Full-screen player (signed in, without the sidebar). */
const playRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/play/$id',
  component: PlayerPage,
  beforeLoad: requireSession,
  // `start` (ms) overrides the resume point; `start=0` plays from the beginning.
  validateSearch: (search: Record<string, unknown>): { start?: number } => {
    const start = Number(search.start)
    return search.start !== undefined && Number.isInteger(start) && start >= 0 ? { start } : {}
  },
})

/** Full-screen live TV player (signed in, without the sidebar). */
const livePlayRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/live/$channelId',
  component: LivePlayerPage,
  beforeLoad: requireSession,
})

const liveRoute = createRoute({
  getParentRoute: () => appRoute,
  path: '/live',
  component: LiveTvPage,
})

/** Public invitation landing page. */
const inviteRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/invite/$code',
  component: InvitePage,
})

const settingsTabs: SettingsTab[] = SETTINGS_TABS.map((t) => t.id)
const settingsRoute = createRoute({
  getParentRoute: () => appRoute,
  path: '/settings',
  component: SettingsPage,
  validateSearch: (search: Record<string, unknown>): { tab?: SettingsTab } =>
    settingsTabs.includes(search.tab as SettingsTab) ? { tab: search.tab as SettingsTab } : {},
})

const browseSorts = ['title', 'date_added', 'year', 'rating'] as const

function validateBrowse(search: Record<string, unknown>): BrowseSearch {
  const out: BrowseSearch = {}
  if (browseSorts.includes(search.sort as never)) out.sort = search.sort as BrowseSearch['sort']
  if (search.order === 'asc' || search.order === 'desc') out.order = search.order
  if (typeof search.genre === 'string' && search.genre) out.genre = search.genre
  if (search.played === 'yes' || search.played === 'no') out.played = search.played
  return out
}

const moviesRoute = createRoute({
  getParentRoute: () => appRoute,
  path: '/movies',
  component: () => <BrowsePage kind="movie" from="/movies" />,
  validateSearch: validateBrowse,
})

const showsRoute = createRoute({
  getParentRoute: () => appRoute,
  path: '/shows',
  component: () => <BrowsePage kind="series" from="/shows" />,
  validateSearch: validateBrowse,
})

const itemRoute = createRoute({
  getParentRoute: () => appRoute,
  path: '/item/$id',
  component: ItemPage,
  validateSearch: (search: Record<string, unknown>): { season?: number } => {
    const season = Number(search.season)
    return Number.isInteger(season) && season >= 0 ? { season } : {}
  },
  loader: ({ context, params }) => context.queryClient.prefetchQuery(itemQuery(Number(params.id))),
})

const personRoute = createRoute({
  getParentRoute: () => appRoute,
  path: '/person/$id',
  component: PersonPage,
})

const collectionsRoute = createRoute({
  getParentRoute: () => appRoute,
  path: '/collections',
  component: CollectionsPage,
})

const collectionRoute = createRoute({
  getParentRoute: () => appRoute,
  path: '/collection/$id',
  component: CollectionPage,
})

const playlistsRoute = createRoute({
  getParentRoute: () => appRoute,
  path: '/playlists',
  component: PlaylistsPage,
})

const playlistRoute = createRoute({
  getParentRoute: () => appRoute,
  path: '/playlist/$id',
  component: PlaylistPage,
})

const searchRoute = createRoute({
  getParentRoute: () => appRoute,
  path: '/search',
  component: SearchPage,
  validateSearch: (search: Record<string, unknown>): { q?: string } =>
    typeof search.q === 'string' && search.q ? { q: search.q } : {},
})

function placeholder(path: string, title: string, note: string) {
  return createRoute({
    getParentRoute: () => appRoute,
    path,
    component: () => <PlaceholderPage title={title} note={note} />,
  })
}

const routeTree = rootRoute.addChildren([
  setupRoute,
  loginRoute,
  profilesRoute,
  playRoute,
  livePlayRoute,
  inviteRoute,
  appRoute.addChildren([
    settingsRoute,
    createRoute({ getParentRoute: () => appRoute, path: '/', component: HomePage }),
    moviesRoute,
    showsRoute,
    itemRoute,
    personRoute,
    collectionsRoute,
    collectionRoute,
    playlistsRoute,
    playlistRoute,
    searchRoute,
    liveRoute,
    placeholder('/music', 'Musik', 'Musikbibliotheken folgen in Phase 9.'),
    createRoute({ getParentRoute: () => appRoute, path: '/for-you', component: ForYouPage }),
    createRoute({ getParentRoute: () => appRoute, path: '/group', component: GroupPage }),
  ]),
])

export function createAppRouter(queryClient: QueryClient) {
  const router = createRouter({
    routeTree,
    context: { queryClient },
    defaultPreload: 'intent',
  })
  // A rejected token sends the user back to sign-in from wherever they are.
  setUnauthorizedHandler(() => {
    const here = router.state.location.href
    void router.navigate({ to: '/login', search: here === '/' ? {} : { redirect: here } })
  })
  return router
}

declare module '@tanstack/react-router' {
  interface Register {
    router: ReturnType<typeof createAppRouter>
  }
}
