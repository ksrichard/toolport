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
      {/* Three API operations join in one node and leave through one plug: the MCP endpoint. */}
      <svg width="24" height="24" viewBox="0 0 24 24" aria-hidden="true">
        <rect x="1" y="3" width="14.5" height="18" rx="4.5" fill="none" stroke="currentColor" strokeWidth="1.6" />
        <path
          d="M5 7.5c3 0 3 4.5 6 4.5M5 16.5c3 0 3-4.5 6-4.5M5 12h6"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.4"
          strokeLinecap="round"
          opacity=".7"
        />
        <circle cx="5" cy="7.5" r="1.4" fill="currentColor" />
        <circle cx="5" cy="12" r="1.4" fill="currentColor" />
        <circle cx="5" cy="16.5" r="1.4" fill="currentColor" />
        <g fill="var(--accent)" stroke="var(--accent)" strokeLinecap="round">
          <path d="M11 12h7.5" strokeWidth="1.8" />
          <rect x="18" y="8" width="3" height="8" rx="1" stroke="none" />
          <path d="M21 10h2M21 14h2" strokeWidth="1.6" />
          <circle cx="11" cy="12" r="2.3" stroke="none" />
        </g>
      </svg>
      ToolPort
    </a>
  )
}
