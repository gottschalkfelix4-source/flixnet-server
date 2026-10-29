/**
 * Copies text. `navigator.clipboard` exists only on HTTPS or localhost; on plain
 * `http://<server-ip>` the old selection-based copy still works.
 */
export async function copyText(text: string): Promise<boolean> {
  if (navigator.clipboard) {
    try {
      await navigator.clipboard.writeText(text)
      return true
    } catch {
      // Fall through to the legacy path (e.g. permission denied).
    }
  }
  const field = document.createElement('textarea')
  field.value = text
  field.setAttribute('readonly', '')
  field.style.position = 'fixed'
  field.style.opacity = '0'
  document.body.append(field)
  field.select()
  try {
    return document.execCommand('copy')
  } finally {
    field.remove()
  }
}
