export type SettingsTab =
  | 'account'
  | 'trakt'
  | 'dashboard'
  | 'libraries'
  | 'metadata'
  | 'subtitles'
  | 'transcoding'
  | 'ai'
  | 'livetv'
  | 'network'
  | 'users'
  | 'invitations'
  | 'sso'
  | 'notifications'
  | 'import'
  | 'backup'
  | 'jobs'
  | 'sessions'
  | 'logs'

export interface SettingsTabInfo {
  id: SettingsTab
  label: string
  admin: boolean
}

/** Groups of the settings sidebar, in order (like Plex: account first, status last). */
export const SETTINGS_GROUPS: { title: string; tabs: SettingsTabInfo[] }[] = [
  {
    title: 'Konto',
    tabs: [
      { id: 'account', label: 'Mein Konto', admin: false },
      { id: 'trakt', label: 'Trakt', admin: false },
    ],
  },
  {
    title: 'Server',
    tabs: [
      { id: 'libraries', label: 'Bibliotheken', admin: true },
      { id: 'metadata', label: 'Metadaten', admin: true },
      { id: 'subtitles', label: 'Untertitel', admin: true },
      { id: 'transcoding', label: 'Transcoding', admin: true },
      { id: 'ai', label: 'KI', admin: true },
      { id: 'livetv', label: 'Live TV', admin: true },
      { id: 'network', label: 'Netzwerk & Geräte', admin: true },
    ],
  },
  {
    title: 'Nutzer & Zugriff',
    tabs: [
      { id: 'users', label: 'Nutzer', admin: true },
      { id: 'invitations', label: 'Einladungen', admin: true },
      { id: 'sso', label: 'Anmeldung (SSO)', admin: true },
    ],
  },
  {
    title: 'Verwaltung',
    tabs: [
      { id: 'notifications', label: 'Benachrichtigungen', admin: true },
      { id: 'import', label: 'Import', admin: true },
      { id: 'backup', label: 'Sicherung', admin: true },
      { id: 'jobs', label: 'Aufgaben', admin: true },
    ],
  },
  {
    title: 'Status',
    tabs: [
      { id: 'dashboard', label: 'Dashboard', admin: true },
      { id: 'sessions', label: 'Wiedergaben', admin: true },
      { id: 'logs', label: 'Protokoll', admin: true },
    ],
  },
]

export const SETTINGS_TABS: SettingsTabInfo[] = SETTINGS_GROUPS.flatMap((g) => g.tabs)
