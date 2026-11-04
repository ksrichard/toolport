// Client tabs and the copyable command. Onboarding and the Tokens page show them once,
// right after they create a token.

import { useState } from 'react'
import { CLIENTS, snippet, WHERE, type Client } from './clientCommand'
import { CopyButton } from './CopyButton'
import type { ServerSummary } from './types/ServerSummary'

export function ClientTabs({ client, onChange }: { client: Client; onChange: (client: Client) => void }) {
  return (
    <div className="skins" role="tablist" aria-label="MCP client">
      {CLIENTS.map((c) => (
        <button key={c} type="button" role="tab" aria-selected={client === c} aria-pressed={client === c} onClick={() => onChange(c)}>
          {c}
        </button>
      ))}
    </div>
  )
}

/** The client tabs and the copyable command, without the connection badge of onboarding. */
export function ClientSnippet({ server, token }: { server: ServerSummary; token: string }) {
  const [client, setClient] = useState<Client>('Claude Code')
  const command = snippet(client, server, token)
  return (
    <>
      <ClientTabs client={client} onChange={setClient} />
      <p className="muted">{WHERE[client]}</p>
      <div className="snippet">
        <pre>{command}</pre>
        <CopyButton text={command} label="Copy" className="btn primary" />
      </div>
    </>
  )
}
