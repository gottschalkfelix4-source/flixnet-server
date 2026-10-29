export type SettingsTab = 'account' | 'libraries' | 'metadata' | 'users' | 'invitations' | 'jobs'

export const SETTINGS_TABS: { id: SettingsTab; label: string; admin: boolean }[] = [
  { id: 'account', label: 'Mein Konto', admin: false },
  { id: 'libraries', label: 'Bibliotheken', admin: true },
  { id: 'metadata', label: 'Metadaten', admin: true },
  { id: 'users', label: 'Nutzer', admin: true },
  { id: 'invitations', label: 'Einladungen', admin: true },
  { id: 'jobs', label: 'Aufgaben', admin: true },
]
