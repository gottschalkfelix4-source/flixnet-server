import { queryOptions, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { api, unwrap, type Schemas } from '../client'

// ---- Dashboard & logs --------------------------------------------------------------

export function useDashboard() {
  return useQuery({
    queryKey: ['ops', 'dashboard'],
    queryFn: async () => unwrap(await api.GET('/api/v1/dashboard')),
    refetchInterval: 3000,
  })
}

export type LogLevel = Schemas['LogLevel']
export type LogLine = Schemas['LogLine']

export async function fetchLogs(level: LogLevel, after: number) {
  return unwrap(await api.GET('/api/v1/logs', { params: { query: { level, after, limit: 2000 } } }))
}

// ---- Notifications -----------------------------------------------------------------

const targetsKey = ['ops', 'notifications', 'targets']

export type NotificationTarget = Schemas['NotificationTargetDto']
export type NotificationTargetInput = Schemas['NotificationTargetInput']

export function useNotificationTargets() {
  return useQuery({
    queryKey: targetsKey,
    queryFn: async () => unwrap(await api.GET('/api/v1/notifications/targets')),
  })
}

export function useNotificationEvents() {
  return useQuery({
    queryKey: ['ops', 'notifications', 'events'],
    queryFn: async () => unwrap(await api.GET('/api/v1/notifications/events')),
    staleTime: Infinity,
  })
}

export function useSaveNotificationTarget() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async ({ id, body }: { id?: number; body: NotificationTargetInput }) =>
      id === undefined
        ? unwrap(await api.POST('/api/v1/notifications/targets', { body }))
        : unwrap(await api.PUT('/api/v1/notifications/targets/{id}', { params: { path: { id } }, body })),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: targetsKey }),
  })
}

export function useDeleteNotificationTarget() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (id: number) =>
      unwrap(await api.DELETE('/api/v1/notifications/targets/{id}', { params: { path: { id } } })),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: targetsKey }),
  })
}

export function useTestNotificationTarget() {
  return useMutation({
    mutationFn: async (id: number) =>
      unwrap(await api.POST('/api/v1/notifications/targets/{id}/test', { params: { path: { id } } })),
  })
}

const smtpKey = ['ops', 'smtp']

export function useSmtpSettings() {
  return useQuery({
    queryKey: smtpKey,
    queryFn: async () => unwrap(await api.GET('/api/v1/settings/smtp')),
  })
}

export function useUpdateSmtpSettings() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (body: Schemas['UpdateSmtpSettings']) =>
      unwrap(await api.PUT('/api/v1/settings/smtp', { body })),
    onSuccess: (data) => queryClient.setQueryData(smtpKey, data),
  })
}

// ---- Backups -----------------------------------------------------------------------

const backupsKey = ['ops', 'backups']

export function useBackups() {
  return useQuery({
    queryKey: backupsKey,
    queryFn: async () => unwrap(await api.GET('/api/v1/backups')),
  })
}

export function useCreateBackup() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async () => unwrap(await api.POST('/api/v1/backups')),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: backupsKey }),
  })
}

export function useUpdateBackupSettings() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (body: Schemas['UpdateBackupSettings']) =>
      unwrap(await api.PUT('/api/v1/settings/backup', { body })),
    onSuccess: (data) => queryClient.setQueryData(backupsKey, data),
  })
}

// ---- History import ----------------------------------------------------------------

export type ImportRequest = Schemas['ImportRequest']
export type ImportResult = Schemas['ImportResultDto']

export function useImport() {
  return useMutation({
    mutationFn: async ({ apply, body }: { apply: boolean; body: ImportRequest }) =>
      apply
        ? unwrap(await api.POST('/api/v1/import/apply', { body }))
        : unwrap(await api.POST('/api/v1/import/preview', { body })),
  })
}

// ---- Trakt -------------------------------------------------------------------------

const traktKey = ['ops', 'trakt']
const traktSettingsKey = ['ops', 'trakt', 'settings']

export function useTraktStatus() {
  return useQuery({
    queryKey: traktKey,
    queryFn: async () => unwrap(await api.GET('/api/v1/trakt')),
  })
}

export function useTraktSettings(enabled: boolean) {
  return useQuery({
    queryKey: traktSettingsKey,
    queryFn: async () => unwrap(await api.GET('/api/v1/settings/trakt')),
    enabled,
  })
}

export function useUpdateTraktSettings() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (body: Schemas['UpdateTraktSettings']) =>
      unwrap(await api.PUT('/api/v1/settings/trakt', { body })),
    onSuccess: (data) => {
      queryClient.setQueryData(traktSettingsKey, data)
      void queryClient.invalidateQueries({ queryKey: traktKey, exact: true })
    },
  })
}

export function useStartTraktConnect() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async () => unwrap(await api.POST('/api/v1/trakt/connect')),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: traktKey, exact: true }),
  })
}

export function usePollTraktConnect() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async () => unwrap(await api.POST('/api/v1/trakt/connect/poll')),
    onSuccess: (data) => queryClient.setQueryData(traktKey, data.status),
  })
}

export function useUpdateTraktOptions() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (body: Schemas['UpdateTraktOptions']) => unwrap(await api.PUT('/api/v1/trakt', { body })),
    onSuccess: (data) => queryClient.setQueryData(traktKey, data),
  })
}

export function useDisconnectTrakt() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async () => unwrap(await api.DELETE('/api/v1/trakt')),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: traktKey, exact: true }),
  })
}

export function useSyncTrakt() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async () => unwrap(await api.POST('/api/v1/trakt/sync')),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: traktKey, exact: true }),
  })
}

// ---- Single sign-on ----------------------------------------------------------------

export const oidcLoginConfigQuery = queryOptions({
  queryKey: ['ops', 'oidc', 'login'],
  queryFn: async () => unwrap(await api.GET('/api/v1/auth/oidc/config')),
  staleTime: 60_000,
})

const oidcSettingsKey = ['ops', 'oidc', 'settings']

export function useOidcSettings() {
  return useQuery({
    queryKey: oidcSettingsKey,
    queryFn: async () => unwrap(await api.GET('/api/v1/settings/oidc')),
  })
}

export function useUpdateOidcSettings() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: async (body: Schemas['UpdateOidcSettings']) =>
      unwrap(await api.PUT('/api/v1/settings/oidc', { body })),
    onSuccess: (data) => {
      queryClient.setQueryData(oidcSettingsKey, data)
      void queryClient.invalidateQueries({ queryKey: oidcLoginConfigQuery.queryKey })
    },
  })
}
