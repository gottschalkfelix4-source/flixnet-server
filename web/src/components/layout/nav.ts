import {
  Clapperboard,
  Film,
  House,
  Music,
  Radio,
  Search,
  Settings,
  Sparkles,
  type LucideIcon,
} from 'lucide-react'

export interface NavItem {
  to: string
  label: string
  icon: LucideIcon
}

export const mainNav: NavItem[] = [
  { to: '/', label: 'Startseite', icon: House },
  { to: '/movies', label: 'Filme', icon: Film },
  { to: '/shows', label: 'Serien', icon: Clapperboard },
  { to: '/music', label: 'Musik', icon: Music },
  { to: '/live', label: 'Live TV', icon: Radio },
  { to: '/for-you', label: 'Für dich', icon: Sparkles },
  { to: '/search', label: 'Suche', icon: Search },
]

export const footerNav: NavItem[] = [{ to: '/settings', label: 'Einstellungen', icon: Settings }]
