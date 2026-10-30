import {
  BookAudio,
  Clapperboard,
  Film,
  House,
  Images,
  Layers,
  ListVideo,
  Music,
  Radio,
  Search,
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
  { to: '/collections', label: 'Sammlungen', icon: Layers },
  { to: '/playlists', label: 'Playlists', icon: ListVideo },
  { to: '/music', label: 'Musik', icon: Music },
  { to: '/photos', label: 'Fotos', icon: Images },
  { to: '/audiobooks', label: 'Hörbücher', icon: BookAudio },
  { to: '/live', label: 'Live TV', icon: Radio },
  { to: '/for-you', label: 'Für dich', icon: Sparkles },
  { to: '/search', label: 'Suche', icon: Search },
]

