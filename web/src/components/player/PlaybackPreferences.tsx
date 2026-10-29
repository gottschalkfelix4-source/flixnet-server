import { useState } from 'react'
import { usePreferences, useUpdatePreferences } from '@/api/queries/segments'
import { FormError } from '@/components/layout/AuthLayout'
import { Checkbox, Section } from '@/components/ui/Section'

/** Personal playback preferences of the signed-in profile (settings → "Mein Konto"). */
export function PlaybackPreferences() {
  const preferences = usePreferences()
  const update = useUpdatePreferences()
  // The box follows the click at once; a failed save shows the stored value again.
  const [pending, setPending] = useState<boolean | null>(null)
  const checked = pending ?? preferences.data?.skip_intro_automatically ?? false
  return (
    <Section title="Wiedergabe" description="Gilt nur für dein Profil.">
      <Checkbox
        label="Intro automatisch überspringen"
        hint="Erkannte Intros und Rückblicke werden ohne Klick übersprungen."
        checked={checked}
        onChange={(value) => {
          setPending(value)
          update.mutate({ skip_intro_automatically: value }, { onSettled: () => setPending(null) })
        }}
      />
      <div className="mt-3">
        <FormError error={update.error} />
      </div>
    </Section>
  )
}
