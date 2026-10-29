import clsx from 'clsx'
import { type InputHTMLAttributes, useId } from 'react'

interface TextFieldProps extends InputHTMLAttributes<HTMLInputElement> {
  label: string
  hint?: string
}

export function TextField({ label, hint, className, ...props }: TextFieldProps) {
  const id = useId()
  const hintId = hint ? `${id}-hint` : undefined
  return (
    <div className={clsx('flex flex-col gap-1.5', className)}>
      <label htmlFor={id} className="text-sm font-semibold text-muted">
        {label}
      </label>
      <input
        id={id}
        aria-describedby={hintId}
        {...props}
        className="h-11 rounded-lg border border-line bg-surface-2 px-3.5 text-fg placeholder:text-subtle focus:border-accent focus:outline-none"
      />
      {hint && (
        <p id={hintId} className="text-xs text-subtle">
          {hint}
        </p>
      )}
    </div>
  )
}
