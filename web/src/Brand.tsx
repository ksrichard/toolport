import type { MouseEvent } from 'react'

/** The logo links to the main page. In the app, `onNavigate` opens it without a page reload. */
export function Brand({ onNavigate }: { onNavigate?: () => void }) {
  const click = (event: MouseEvent<HTMLAnchorElement>) => {
    // A modifier key or a middle click keeps the browser behaviour, for example a new tab.
    if (!onNavigate || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return
    event.preventDefault()
    onNavigate()
  }
  return (
    <a className="brand" href="/" aria-label="ToolPort home" onClick={click}>
      <svg width="22" height="22" viewBox="0 0 22 22" aria-hidden="true">
        <rect x="1" y="1" width="20" height="20" rx="5" fill="none" stroke="currentColor" strokeWidth="1.6" />
        <path d="M6 8h10M6 14h4M13 14h3" stroke="var(--accent)" strokeWidth="1.8" strokeLinecap="round" />
      </svg>
      ToolPort
    </a>
  )
}
