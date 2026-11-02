import { useEffect, useState } from 'react'

interface CopyButtonProps {
  text: string
  label?: string
  className?: string
}

export function CopyButton({ text, label = 'Copy', className = 'btn' }: CopyButtonProps) {
  const [state, setState] = useState<'idle' | 'copied' | 'failed'>('idle')

  useEffect(() => {
    if (state === 'idle') return
    const id = setTimeout(() => setState('idle'), 2000)
    return () => clearTimeout(id)
  }, [state])

  const copy = () => {
    // The clipboard API needs a secure context. Plain http on a LAN address does not have one.
    if (!navigator.clipboard) {
      setState('failed')
      return
    }
    navigator.clipboard.writeText(text).then(
      () => setState('copied'),
      () => setState('failed'),
    )
  }

  return (
    <button className={className} type="button" onClick={copy} aria-live="polite">
      {state === 'copied' ? 'Copied' : state === 'failed' ? 'Select and copy' : label}
    </button>
  )
}
