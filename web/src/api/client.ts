import createClient, { type Middleware } from 'openapi-fetch'
import { getSession, setSession } from '@/lib/session'
import type { components, paths } from './schema'

/** Typed client for the Flixnet API, generated from `openapi.json` (`npm run gen:api`). */
export const api = createClient<paths>({ baseUrl: '/' })

export type Schemas = components['schemas']

/** Called when the server rejects our token (revoked, expired, user deleted). */
let onUnauthorized: () => void = () => {}
export function setUnauthorizedHandler(handler: () => void) {
  onUnauthorized = handler
}

const auth: Middleware = {
  onRequest({ request }) {
    const session = getSession()
    if (session) request.headers.set('Authorization', `Bearer ${session.token}`)
    return request
  },
  onResponse({ request, response }) {
    if (response.status === 401 && request.headers.has('Authorization')) {
      setSession(null)
      onUnauthorized()
    }
    return response
  },
}
api.use(auth)

/** An API error with the server's machine-readable code and German message. */
export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(message)
  }
}

/** Unwraps an openapi-fetch result, throwing [`ApiError`] so TanStack Query sees failures. */
export function unwrap<T>(result: { data?: T; error?: unknown; response: Response }): T {
  if (result.response.ok) return result.data as T
  const body = result.error as Partial<Schemas['ErrorBody']> | undefined
  throw new ApiError(
    result.response.status,
    body?.code ?? 'http_error',
    body?.message ?? `Anfrage fehlgeschlagen (${result.response.status}).`,
  )
}
