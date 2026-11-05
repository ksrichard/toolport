// The command or config that connects an MCP client to a server with a token.

import type { ServerSummary } from './types/ServerSummary'

export const CLIENTS = ['Claude Code', 'Cursor', 'Claude Desktop', 'VS Code', 'Windsurf', 'Codex', 'Gemini CLI'] as const
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
    case 'VS Code': {
      // Server names are lowercase letters, digits, and dashes, so single quotes are safe in a shell.
      const config = { name: server.name, type: 'http', url: server.mcp_url, headers: { Authorization: auth } }
      return `code --add-mcp '${JSON.stringify(config)}'`
    }
    case 'Windsurf':
      return JSON.stringify({ mcpServers: { [server.name]: { serverUrl: server.mcp_url, headers: { Authorization: auth } } } }, null, 2)
    case 'Codex':
      return [
        `[mcp_servers.${server.name}]`,
        `url = "${server.mcp_url}"`,
        `http_headers = { Authorization = "${auth}" }`,
      ].join('\n')
    case 'Gemini CLI':
      return `gemini mcp add --transport http ${server.name} ${server.mcp_url} --header "Authorization: ${auth}"`
  }
}

export const WHERE: Record<Client, string> = {
  'Claude Code': 'Run this command in a terminal.',
  Cursor: 'Add this to ~/.cursor/mcp.json, then reload Cursor.',
  'Claude Desktop': 'Add this to claude_desktop_config.json, then restart Claude Desktop.',
  'VS Code': 'Run this command in a terminal. VS Code adds the server to your user MCP settings.',
  Windsurf: 'Add this to ~/.codeium/windsurf/mcp_config.json, then refresh the MCP servers in Windsurf.',
  Codex: 'Add this to ~/.codex/config.toml, then restart Codex.',
  'Gemini CLI': 'Run this command in a terminal.',
}
