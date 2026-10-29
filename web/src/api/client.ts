import createClient from 'openapi-fetch'
import type { components, paths } from './schema'

/** Typed client for the Flixnet API, generated from `openapi.json` (`npm run gen:api`). */
export const api = createClient<paths>({ baseUrl: '/' })

export type Schemas = components['schemas']

/** Unwraps an openapi-fetch result, throwing on HTTP errors so TanStack Query sees them. */
export function unwrap<T>(result: { data?: T; error?: unknown; response: Response }): T {
  if (result.data === undefined) {
    throw new Error(`API ${result.response.status} ${result.response.url}`)
  }
  return result.data
}
