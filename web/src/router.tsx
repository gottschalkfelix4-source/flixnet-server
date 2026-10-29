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
import { getSession } from '@/lib/session'
import { HomePage } from '@/routes/HomePage'
import { LoginPage } from '@/routes/LoginPage'
import { NotFoundPage } from '@/routes/NotFoundPage'
import { PlaceholderPage } from '@/routes/PlaceholderPage'
import { SetupPage } from '@/routes/SetupPage'

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
  beforeLoad: async ({ context, location }) => {
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
  },
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
  appRoute.addChildren([
    createRoute({ getParentRoute: () => appRoute, path: '/', component: HomePage }),
    placeholder('/movies', 'Filme', 'Filmbibliotheken kommen mit dem Scanner in Phase 2.'),
    placeholder('/shows', 'Serien', 'Serienbibliotheken kommen mit dem Scanner in Phase 2.'),
    placeholder('/music', 'Musik', 'Musikbibliotheken folgen in Phase 9.'),
    placeholder('/live', 'Live TV', 'Live TV und DVR folgen in Phase 7.'),
    placeholder('/for-you', 'Für dich', 'KI-Empfehlungen folgen in Phase 6.'),
    placeholder('/search', 'Suche', 'Die Suche kommt mit der Items-API in Phase 2.'),
    placeholder('/settings', 'Einstellungen', 'Nutzer- und Serververwaltung kommen in Phase 1.2.'),
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
