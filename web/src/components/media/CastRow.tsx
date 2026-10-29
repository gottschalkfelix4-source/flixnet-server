import { Link } from '@tanstack/react-router'
import { User } from 'lucide-react'
import type { Schemas } from '@/api/client'

type Person = Schemas['PersonDto']

const ROLE_LABELS: Record<string, string> = {
  director: 'Regie',
  writer: 'Drehbuch',
  creator: 'Idee',
  producer: 'Produktion',
}

/** Round avatars with name and character/role; links to the person page. */
export function CastRow({ people }: { people: Person[] }) {
  if (people.length === 0) return null
  return (
    <section className="mb-10" aria-label="Besetzung">
      <h2 className="mb-4 px-6 text-lg font-bold md:px-10">Besetzung & Crew</h2>
      <ul className="flex gap-5 overflow-x-auto px-6 pb-2 md:px-10" data-testid="cast-row">
        {people.slice(0, 30).map((person, i) => (
          <li key={`${person.person_id}-${person.role}-${i}`} className="w-24 shrink-0 text-center">
            <Link to="/person/$id" params={{ id: String(person.person_id) }} className="group block">
              <span className="mx-auto mb-2 flex size-24 items-center justify-center overflow-hidden rounded-full bg-surface-2 ring-accent transition group-hover:ring-2">
                {person.image_url ? (
                  <img src={person.image_url} alt="" loading="lazy" className="size-full object-cover" />
                ) : (
                  <User className="size-9 text-subtle" aria-hidden="true" />
                )}
              </span>
              <span className="line-clamp-2 text-xs font-semibold group-hover:text-white">{person.name}</span>
              <span className="line-clamp-2 text-[11px] text-subtle">
                {person.role === 'actor' ? person.character : (ROLE_LABELS[person.role] ?? person.role)}
              </span>
            </Link>
          </li>
        ))}
      </ul>
    </section>
  )
}
