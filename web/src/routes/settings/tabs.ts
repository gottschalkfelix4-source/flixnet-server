export type SettingsTab =
  | 'account'
  | 'trakt'
  | 'dashboard'
  | 'libraries'
  | 'metadata'
  | 'subtitles'
  | 'transcoding'
  | 'livetv'
  | 'users'
  | 'invitations'
  | 'sso'
  | 'notifications'
  | 'import'
  | 'backup'
  | 'jobs'
  | 'sessions'
  | 'logs'

export const SETTINGS_TABS: { id: SettingsTab; label: string; admin: boolean }[] = [
  { id: 'account', label: 'Mein Konto', admin: false },
  { id: 'trakt', label: 'Trakt', admin: false },
  { id: 'dashboard', label: 'Übersicht', admin: true },
  { id: 'libraries', label: 'Bibliotheken', admin: true },
  { id: 'metadata', label: 'Metadaten', admin: true },
  { id: 'subtitles', label: 'Untertitel', admin: true },
  { id: 'transcoding', label: 'Transcoding', admin: true },
  { id: 'livetv', label: 'Live TV', admin: true },
  { id: 'users', label: 'Nutzer', admin: true },
  { id: 'invitations', label: 'Einladungen', admin: true },
  { id: 'sso', label: 'Anmeldung (SSO)', admin: true },
  { id: 'notifications', label: 'Benachrichtigungen', admin: true },
  { id: 'import', label: 'Import', admin: true },
  { id: 'backup', label: 'Sicherung', admin: true },
  { id: 'jobs', label: 'Aufgaben', admin: true },
  { id: 'sessions', label: 'Wiedergaben', admin: true },
  { id: 'logs', label: 'Protokoll', admin: true },
]
