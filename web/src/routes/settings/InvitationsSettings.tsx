import { Check, Copy, Link2, Trash2 } from 'lucide-react'
import { type FormEvent, useState } from 'react'
import { useCreateInvitation, useDeleteInvitation, useInvitations } from '@/api/queries/users'
import { FormError } from '@/components/layout/AuthLayout'
import { Button } from '@/components/ui/Button'
import { Section, SelectField } from '@/components/ui/Section'
import { TextField } from '@/components/ui/TextField'
import { copyText } from '@/lib/clipboard'
import { AGE_OPTIONS, ageLabel, formatDateTime } from '@/lib/format'

export function InvitationsSettings() {
  const invitations = useInvitations()
  const create = useCreateInvitation()
  const remove = useDeleteInvitation()
  const [note, setNote] = useState('')
  const [age, setAge] = useState('')
  const [days, setDays] = useState('7')
  const [link, setLink] = useState<string | null>(null)
  const [copied, setCopied] = useState(false)
  // Captured once per mount: render must stay pure.
  const [now] = useState(() => Date.now())

  function submit(event: FormEvent) {
    event.preventDefault()
    create.mutate(
      {
        note: note.trim() || null,
        max_age_rating: age === '' ? null : Number(age),
        valid_days: Number(days),
      },
      {
        onSuccess: (created) => {
          setLink(`${window.location.origin}/invite/${created.code}`)
          setCopied(false)
          setNote('')
        },
      },
    )
  }

  async function copy() {
    if (!link) return
    setCopied(await copyText(link))
  }

  return (
    <div className="flex flex-col gap-6">
      <Section
        title="Einladung erstellen"
        description="Der Link gilt für genau ein neues Konto. Er wird nur einmal angezeigt."
      >
        <form onSubmit={submit} className="grid gap-4 md:grid-cols-3">
          <TextField label="Notiz" placeholder="z. B. Für Oma" value={note} maxLength={200} onChange={(e) => setNote(e.target.value)} />
          <SelectField
            label="Altersfreigabe"
            value={age}
            onChange={setAge}
            options={AGE_OPTIONS.map((o) => ({ value: o.value?.toString() ?? '', label: o.label }))}
          />
          <SelectField
            label="Gültig für"
            value={days}
            onChange={setDays}
            options={[
              { value: '1', label: '1 Tag' },
              { value: '7', label: '7 Tage' },
              { value: '30', label: '30 Tage' },
            ]}
          />
          <div className="md:col-span-3">
            <FormError error={create.error} />
            <Button type="submit" variant="accent" loading={create.isPending}>
              <Link2 className="size-4" aria-hidden="true" />
              Link erstellen
            </Button>
          </div>
        </form>
        {link && (
          <div className="mt-5 flex flex-wrap items-center gap-3 rounded-lg border border-accent/40 bg-accent/10 p-3">
            <code data-testid="invite-link" className="min-w-0 flex-1 truncate text-sm">
              {link}
            </code>
            <Button variant="ghost" className="h-9 px-4" onClick={copy}>
              {copied ? <Check className="size-4" /> : <Copy className="size-4" />}
              {copied ? 'Kopiert' : 'Kopieren'}
            </Button>
          </div>
        )}
      </Section>

      <Section title="Einladungen">
        {invitations.data?.length === 0 && <p className="text-sm text-muted">Noch keine Einladungen.</p>}
        <ul className="divide-y divide-line/60">
          {invitations.data?.map((inv) => {
            const state = inv.used_at
              ? 'Eingelöst'
              : inv.expires_at < now
                ? 'Abgelaufen'
                : `Gültig bis ${formatDateTime(inv.expires_at)}`
            return (
              <li key={inv.id} className="flex items-center gap-4 py-3">
                <div className="min-w-0 flex-1">
                  <p className="truncate text-sm font-semibold">{inv.note ?? 'Einladung'}</p>
                  <p className="text-xs text-subtle">
                    {state} · {ageLabel(inv.max_age_rating)}
                  </p>
                </div>
                <Button
                  variant="ghost"
                  className="h-9 px-3"
                  aria-label="Einladung löschen"
                  onClick={() => remove.mutate(inv.id)}
                  loading={remove.isPending && remove.variables === inv.id}
                >
                  <Trash2 className="size-4" />
                </Button>
              </li>
            )
          })}
        </ul>
      </Section>
    </div>
  )
}
