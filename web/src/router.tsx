import { createRootRoute, createRoute, createRouter } from '@tanstack/react-router'
import { AppShell } from '@/components/layout/AppShell'
import { HomePage } from '@/routes/HomePage'
import { NotFoundPage } from '@/routes/NotFoundPage'
import { PlaceholderPage } from '@/routes/PlaceholderPage'

const rootRoute = createRootRoute({
  component: AppShell,
  notFoundComponent: NotFoundPage,
})

function placeholder(path: string, title: string, note: string) {
  return createRoute({
    getParentRoute: () => rootRoute,
    path,
    component: () => <PlaceholderPage title={title} note={note} />,
  })
}

const routeTree = rootRoute.addChildren([
  createRoute({ getParentRoute: () => rootRoute, path: '/', component: HomePage }),
  placeholder('/movies', 'Filme', 'Filmbibliotheken kommen mit dem Scanner in Phase 2.'),
  placeholder('/shows', 'Serien', 'Serienbibliotheken kommen mit dem Scanner in Phase 2.'),
  placeholder('/music', 'Musik', 'Musikbibliotheken folgen in Phase 9.'),
  placeholder('/live', 'Live TV', 'Live TV und DVR folgen in Phase 7.'),
  placeholder('/for-you', 'Für dich', 'KI-Empfehlungen folgen in Phase 6.'),
  placeholder('/search', 'Suche', 'Die Suche kommt mit der Items-API in Phase 2.'),
  placeholder('/settings', 'Einstellungen', 'Einstellungen und Nutzerverwaltung kommen in Phase 1.'),
])

export const router = createRouter({ routeTree, defaultPreload: 'intent' })

declare module '@tanstack/react-router' {
  interface Register {
    router: typeof router
  }
}
