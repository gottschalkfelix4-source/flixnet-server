import { queryOptions, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { deviceInfo, setSession } from '@/lib/session'
import { api, unwrap, type Schemas } from '../client'

// ---- Profiles ---------------------------------------------------------------

export function useProfiles() {
  return useQuery({
    queryKey: ['profiles'],
    queryFn: async () => unwrap(await api.GET('/api/v1/auth/profiles')),
  })
}

export function useSwitchProfile() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (body: { user_id: number; pin?: string; password?: string }) =>
      unwrap(await api.POST('/api/v1/auth/switch', { body: { ...body, device: deviceInfo() } })),
    onSuccess: (session) => {
      // Everything cached belongs to the previous profile.
      queryClient.clear()
      setSession(session)
    },
  })
}

// ---- Own account ------------------------------------------------------------

export function useChangePassword() {
  return useMutation({
    mutationFn: async (body: Schemas['ChangePasswordRequest']) =>
      unwrap(await api.PUT('/api/v1/users/me/password', { body })),
  })
}

export function useSetPin() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (pin: string | null) =>
      unwrap(await api.PUT('/api/v1/users/me/pin', { body: { pin } })),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['users', 'me'] }),
  })
}

const devicesQuery = queryOptions({
  queryKey: ['users', 'me', 'devices'],
  queryFn: async () => unwrap(await api.GET('/api/v1/users/me/devices')),
})

export function useMyDevices() {
  return useQuery(devicesQuery)
}

export function useSignOutDevice() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (id: number) =>
      unwrap(await api.DELETE('/api/v1/users/me/devices/{id}', { params: { path: { id } } })),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: devicesQuery.queryKey }),
  })
}

// ---- User administration ----------------------------------------------------

const usersQuery = queryOptions({
  queryKey: ['users', 'all'],
  queryFn: async () => unwrap(await api.GET('/api/v1/users')),
})

export function useUsers() {
  return useQuery(usersQuery)
}

function useInvalidateUsers() {
  const queryClient = useQueryClient()
  return () =>
    Promise.all([
      queryClient.invalidateQueries({ queryKey: usersQuery.queryKey }),
      queryClient.invalidateQueries({ queryKey: ['profiles'] }),
    ])
}

export function useCreateUser() {
  const invalidate = useInvalidateUsers()
  return useMutation({
    mutationFn: async (body: Schemas['CreateUserRequest']) =>
      unwrap(await api.POST('/api/v1/users', { body })),
    onSuccess: invalidate,
  })
}

export function useUpdateUser() {
  const invalidate = useInvalidateUsers()
  return useMutation({
    mutationFn: async ({ id, ...body }: Schemas['UpdateUserRequest'] & { id: number }) =>
      unwrap(await api.PATCH('/api/v1/users/{id}', { params: { path: { id } }, body })),
    onSuccess: invalidate,
  })
}

export function useDeleteUser() {
  const invalidate = useInvalidateUsers()
  return useMutation({
    mutationFn: async (id: number) =>
      unwrap(await api.DELETE('/api/v1/users/{id}', { params: { path: { id } } })),
    onSuccess: invalidate,
  })
}

// ---- Invitations --------------------------------------------------------------

const invitationsQuery = queryOptions({
  queryKey: ['invitations'],
  queryFn: async () => unwrap(await api.GET('/api/v1/invitations')),
})

export function useInvitations() {
  return useQuery(invitationsQuery)
}

export function useCreateInvitation() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (body: Schemas['CreateInvitationRequest']) =>
      unwrap(await api.POST('/api/v1/invitations', { body })),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: invitationsQuery.queryKey }),
  })
}

export function useDeleteInvitation() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (id: number) =>
      unwrap(await api.DELETE('/api/v1/invitations/{id}', { params: { path: { id } } })),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: invitationsQuery.queryKey }),
  })
}

export function invitationStatusQuery(code: string) {
  return queryOptions({
    queryKey: ['invitations', 'status', code],
    queryFn: async () =>
      unwrap(
        await api.GET('/api/v1/invitations/redeem/{code}', { params: { path: { code } } }),
      ),
  })
}

export function useRedeemInvitation(code: string) {
  return useMutation({
    mutationFn: async ({ name, password }: { name: string; password: string }) =>
      unwrap(
        await api.POST('/api/v1/invitations/redeem/{code}', {
          params: { path: { code } },
          body: { name, password, device: deviceInfo() },
        }),
      ),
    onSuccess: (session) => setSession(session),
  })
}
