import { EyeOff, Lock, Plus, ShieldCheck, Trash2 } from 'lucide-react'
import { type FormEvent, useState } from 'react'
import type { Schemas } from '@/api/client'
import { useGenres } from '@/api/queries/items'
import { useLibraries, useLibraryAccess, useSetLibraryAccess } from '@/api/queries/libraries'
import { useCreateUser, useDeleteUser, useUpdateUser, useUsers } from '@/api/queries/users'
import { FormError } from '@/components/layout/AuthLayout'
import { Avatar } from '@/components/ui/Avatar'
import { Button } from '@/components/ui/Button'
import { Dialog } from '@/components/ui/Dialog'
import { Checkbox, Section, SelectField } from '@/components/ui/Section'
import { TextField } from '@/components/ui/TextField'
import { AGE_OPTIONS, ageLabel } from '@/lib/format'
import { useSession } from '@/lib/session'

type User = Schemas['UserDto']

export function UsersSettings() {
  const users = useUsers()
  const [editing, setEditing] = useState<User | 'new' | null>(null)

  return (
    <Section
      title="Nutzer & Profile"
      description="Profile ohne Passwort können sich nicht von außen anmelden – man wechselt auf einem angemeldeten Gerät zu ihnen."
      action={
        <Button variant="accent" className="h-10" onClick={() => setEditing('new')}>
          <Plus className="size-4" aria-hidden="true" />
          Nutzer anlegen
        </Button>
      }
    >
      <ul className="divide-y divide-line/60">
        {users.data?.map((user) => (
          <li key={user.id}>
            <button
              type="button"
              onClick={() => setEditing(user)}
              className="flex w-full items-center gap-4 rounded-lg px-2 py-3 text-left hover:bg-surface-2"
            >
              <Avatar id={user.id} name={user.name} size="sm" />
              <div className="min-w-0 flex-1">
                <p className="flex items-center gap-2 truncate text-sm font-bold">
                  {user.name}
                  {user.is_admin && <ShieldCheck className="size-4 text-accent" aria-label="Administrator" />}
                  {user.has_pin && <Lock className="size-3.5 text-muted" aria-label="PIN" />}
                  {user.is_hidden && <EyeOff className="size-3.5 text-muted" aria-label="Versteckt" />}
                </p>
                <p className="text-xs text-subtle">
                  {user.is_admin ? 'Administrator' : 'Mitglied'} · {ageLabel(user.max_age_rating)}
                  {user.blocked_genres.length > 0 &&
                    ` · ${user.blocked_genres.length} ${user.blocked_genres.length === 1 ? 'Genre' : 'Genres'} gesperrt`}
                  {!user.has_password && ' · nur Profil'}
                </p>
              </div>
            </button>
          </li>
        ))}
      </ul>
      {editing && (
        <UserDialog
          key={editing === 'new' ? 'new' : editing.id}
          user={editing === 'new' ? null : editing}
          onClose={() => setEditing(null)}
        />
      )}
    </Section>
  )
}

/** Genres to hide from a profile: every genre of the libraries plus already blocked ones. */
function GenrePicker({ selected, onChange }: { selected: string[]; onChange: (genres: string[]) => void }) {
  const genres = useGenres()
  const names = [...new Set([...(genres.data?.map((g) => g.name) ?? []), ...selected])].sort((a, b) =>
    a.localeCompare(b, 'de'),
  )
  if (names.length === 0) return null
  return (
    <div className="flex flex-col gap-2">
      <p className="text-sm font-semibold">Gesperrte Genres</p>
      <div className="flex flex-wrap gap-2" role="group" aria-label="Gesperrte Genres">
        {names.map((name) => {
          const on = selected.includes(name)
          return (
            <button
              key={name}
              type="button"
              aria-pressed={on}
              onClick={() => onChange(on ? selected.filter((g) => g !== name) : [...selected, name])}
              className={
                on
                  ? 'rounded-full border border-red-400/60 bg-red-500/15 px-3 py-1 text-xs font-semibold text-red-300'
                  : 'rounded-full border border-line px-3 py-1 text-xs font-semibold text-muted hover:text-fg'
              }
            >
              {name}
            </button>
          )
        })}
      </div>
      <p className="text-xs text-subtle">Gesperrte Genres gelten auch für die Staffeln und Folgen einer Serie.</p>
    </div>
  )
}

function UserDialog({ user, onClose }: { user: User | null; onClose: () => void }) {
  const session = useSession()
  const create = useCreateUser()
  const update = useUpdateUser()
  const remove = useDeleteUser()
  const [name, setName] = useState(user?.name ?? '')
  const [password, setPassword] = useState('')
  const [pin, setPin] = useState('')
  const [isAdmin, setIsAdmin] = useState(user?.is_admin ?? false)
  const [isHidden, setIsHidden] = useState(user?.is_hidden ?? false)
  const [age, setAge] = useState<string>(user?.max_age_rating?.toString() ?? '')
  const [allowUnrated, setAllowUnrated] = useState(user?.allow_unrated ?? false)
  const [blockedGenres, setBlockedGenres] = useState<string[]>(user?.blocked_genres ?? [])
  const [confirmDelete, setConfirmDelete] = useState(false)
  // Library access: loaded values until the admin changes something.
  const libraries = useLibraries()
  const loadedAccess = useLibraryAccess(user?.id ?? null)
  const [accessDraft, setAccessDraft] = useState<Schemas['LibraryAccess'] | null>(null)
  const access = accessDraft ?? loadedAccess.data
  const setAccess = useSetLibraryAccess()
  const isSelf = user?.id === session?.user.id
  const error = create.error ?? update.error ?? remove.error ?? setAccess.error

  function submit(event: FormEvent) {
    event.preventDefault()
    const maxAge = age === '' ? null : Number(age)
    if (user) {
      update.mutate(
        {
          id: user.id,
          name: name.trim(),
          is_admin: isAdmin,
          is_hidden: isHidden,
          max_age_rating: maxAge,
          allow_unrated: allowUnrated,
          blocked_genres: blockedGenres,
          ...(password ? { password } : {}),
          ...(pin ? { pin } : {}),
        },
        {
          onSuccess: () =>
            accessDraft
              ? setAccess.mutate({ id: user.id, ...accessDraft }, { onSuccess: onClose })
              : onClose(),
        },
      )
    } else {
      create.mutate(
        {
          name: name.trim(),
          password: password || null,
          pin: pin || null,
          is_admin: isAdmin,
          is_hidden: isHidden,
          max_age_rating: maxAge,
          allow_unrated: allowUnrated,
          blocked_genres: blockedGenres,
        },
        { onSuccess: onClose },
      )
    }
  }

  return (
    <Dialog
      open
      onOpenChange={(open) => !open && onClose()}
      title={user ? `${user.name} bearbeiten` : 'Nutzer anlegen'}
    >
      <form onSubmit={submit} className="flex flex-col gap-4">
        <TextField label="Name" value={name} maxLength={64} onChange={(e) => setName(e.target.value)} />
        <TextField
          label={user?.has_password ? 'Neues Passwort' : 'Passwort'}
          type="password"
          autoComplete="new-password"
          hint={user ? 'Leer lassen, um es nicht zu ändern.' : 'Optional – ohne Passwort ist es ein reines Profil.'}
          value={password}
          onChange={(e) => setPassword(e.target.value)}
        />
        <TextField
          label={user?.has_pin ? 'Neue PIN' : 'PIN'}
          type="password"
          inputMode="numeric"
          autoComplete="off"
          maxLength={8}
          hint="Optional, 4–8 Ziffern. Schützt den Profilwechsel."
          value={pin}
          onChange={(e) => setPin(e.target.value.replace(/\D/g, ''))}
        />
        <fieldset className="flex flex-col gap-3 rounded-lg border border-line p-3">
          <legend className="px-1 text-sm font-semibold text-muted">Jugendschutz</legend>
          <SelectField
            label="Altersfreigabe"
            value={age}
            onChange={setAge}
            options={AGE_OPTIONS.map((o) => ({ value: o.value?.toString() ?? '', label: o.label }))}
          />
          {age !== '' && (
            <Checkbox
              label="Nicht eingestufte Inhalte erlauben"
              hint="Sonst bleiben Filme und Serien ohne Einstufung für dieses Profil verborgen."
              checked={allowUnrated}
              onChange={setAllowUnrated}
            />
          )}
          <GenrePicker selected={blockedGenres} onChange={setBlockedGenres} />
        </fieldset>
        <Checkbox label="Administrator" checked={isAdmin} onChange={setIsAdmin} />
        <Checkbox
          label="In der Profilauswahl verstecken"
          hint="Für Konten, die nur von eigenen Geräten genutzt werden."
          checked={isHidden}
          onChange={setIsHidden}
        />
        {user && access && !isAdmin && (libraries.data?.length ?? 0) > 0 && (
          <fieldset className="flex flex-col gap-2 rounded-lg border border-line p-3">
            <legend className="px-1 text-sm font-semibold text-muted">Bibliotheken</legend>
            <Checkbox
              label="Alle Bibliotheken"
              hint="Auch solche, die später hinzukommen."
              checked={access.all_libraries}
              onChange={(all) => setAccessDraft({ ...access, all_libraries: all })}
            />
            {!access.all_libraries &&
              libraries.data?.map((library) => (
                <Checkbox
                  key={library.id}
                  label={library.name}
                  checked={access.library_ids.includes(library.id)}
                  onChange={(on) =>
                    setAccessDraft({
                      ...access,
                      library_ids: on
                        ? [...access.library_ids, library.id]
                        : access.library_ids.filter((id) => id !== library.id),
                    })
                  }
                />
              ))}
          </fieldset>
        )}
        <FormError error={error} />
        <div className="flex flex-wrap items-center justify-between gap-3 pt-2">
          {user && !isSelf ? (
            confirmDelete ? (
              <Button
                type="button"
                className="bg-red-500 text-white hover:bg-red-400"
                loading={remove.isPending}
                onClick={() => remove.mutate(user.id, { onSuccess: onClose })}
              >
                Endgültig löschen
              </Button>
            ) : (
              <Button type="button" variant="ghost" onClick={() => setConfirmDelete(true)}>
                <Trash2 className="size-4" aria-hidden="true" />
                Löschen
              </Button>
            )
          ) : (
            <span />
          )}
          <Button
            type="submit"
            variant="accent"
            loading={create.isPending || update.isPending || setAccess.isPending}
            disabled={!name.trim()}
          >
            Speichern
          </Button>
        </div>
      </form>
    </Dialog>
  )
}
