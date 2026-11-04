// The command or config that connects an MCP client to a server with a token.

import type { ServerSummary } from './types/ServerSummary'

export const CLIENTS = ['Claude Code', 'Cursor', 'Claude Desktop'] as const
export type Client = (typeof CLIENTS)[number]

export function snippet(client: Client, server: ServerSummary, token: string): string {
  const auth = `Bearer ${token}`
  switch (client) {
    case 'Claude Code':
      return `claude mcp add --transport http ${server.name} ${server.mcp_url} --header "Authorization: ${auth}"`
    case 'Cursor':
      return JSON.stringify({ mcpServers: { [server.name]: { url: server.mcp_url, headers: { Authorization: auth } } } }, null, 2)
    case 'Claude Desktop':
      // Claude Desktop config files start only local servers. mcp-remote forwards to the URL.
      return JSON.stringify(
        {
          mcpServers: {
            [server.name]: {
              command: 'npx',
              args: ['-y', 'mcp-remote', server.mcp_url, '--header', 'Authorization:${AUTH}'],
              env: { AUTH: auth },
            },
          },
        },
        null,
        2,
      )
  }
}

export const WHERE: Record<Client, string> = {
  'Claude Code': 'Run this command in a terminal.',
  Cursor: 'Add this to ~/.cursor/mcp.json, then reload Cursor.',
  'Claude Desktop': 'Add this to claude_desktop_config.json, then restart Claude Desktop.',
}
