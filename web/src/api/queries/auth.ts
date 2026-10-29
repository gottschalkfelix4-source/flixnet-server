import { queryOptions, useMutation, useQueryClient } from '@tanstack/react-query'
import { deviceInfo, getSession, setSession } from '@/lib/session'
import { api, unwrap } from '../client'

export const setupStatusQuery = queryOptions({
  queryKey: ['setup', 'status'],
  queryFn: async () => unwrap(await api.GET('/api/v1/setup/status')),
  staleTime: Infinity,
})

/**
 * The signed-in user, verified against the server. Also refreshes the user stored in
 * the session (e.g. after a rename); a revoked token fails here with 401.
 */
export const currentUserQuery = queryOptions({
  queryKey: ['users', 'me'],
  queryFn: async () => {
    const user = unwrap(await api.GET('/api/v1/users/me'))
    const session = getSession()
    if (session) setSession({ ...session, user })
    return user
  },
  staleTime: 5 * 60_000,
  retry: false,
})

interface Credentials {
  name: string
  password: string
}

export function useCompleteSetup() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async ({ name, password }: Credentials) =>
      unwrap(await api.POST('/api/v1/setup', { body: { name, password, device: deviceInfo() } })),
    onSuccess: (session) => {
      setSession(session)
      queryClient.setQueryData(setupStatusQuery.queryKey, { setup_required: false })
    },
  })
}

export function useLogin() {
  return useMutation({
    mutationFn: async ({ name, password }: Credentials) =>
      unwrap(
        await api.POST('/api/v1/auth/login', { body: { name, password, device: deviceInfo() } }),
      ),
    onSuccess: (session) => setSession(session),
  })
}

export function useLogout() {
  const queryClient = useQueryClient()
  return useMutation({
    // Sign out locally even if the server is unreachable.
    mutationFn: async () => {
      await api.POST('/api/v1/auth/logout').catch(() => undefined)
    },
    onSettled: () => {
      setSession(null)
      queryClient.clear()
    },
  })
}
