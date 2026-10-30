let last = '/'

/** Remembers the last page outside the settings (the target of their "Zurück"). */
export function rememberMediaPath(href: string) {
  last = href
}

export function lastMediaPath(): string {
  return last
}
