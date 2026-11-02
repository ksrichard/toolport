// The access JWT lives only in this module's memory. The refresh token is an HttpOnly cookie
// that JavaScript cannot read. The browser sends it only to /auth/*.

import type { ImportApi } from './types/ImportApi'
import type { ImportedApi } from './types/ImportedApi'
import type { NewToken } from './types/NewToken'
import type { ServerSummary } from './types/ServerSummary'
import type { Template } from './types/Template'
import type { Theme } from './types/Theme'
import type { ToolSummary } from './types/ToolSummary'
import type { User } from './types/User'

export type { Theme, User }

export class ApiError extends Error {
  readonly status: number
  readonly code: string

  constructor(status: number, code: string, message: string) {
    super(message)
    this.status = status
    this.code = code
  }
}

let accessToken: string | null = null
let refreshing: Promise<boolean> | null = null

/** Gets a new access token. Parallel callers share one request, because each refresh
 * rotates the cookie and a second request with the old cookie would fail. */
export function refreshSession(): Promise<boolean> {
  refreshing ??= (async () => {
    try {
      const res = await fetch('/auth/refresh', { method: 'POST' })
      if (!res.ok) {
        accessToken = null
        return false
      }
      const body: { access_token: string } = await res.json()
      accessToken = body.access_token
      return true
    } catch {
      accessToken = null
      return false
    } finally {
      refreshing = null
    }
  })()
  return refreshing
}

export async function signOut(): Promise<void> {
  accessToken = null
  await fetch('/auth/logout', { method: 'POST' })
}

/** Calls the API with the Bearer token. On a 401 it refreshes the token once and retries. */
export async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  if (!accessToken) await refreshSession()
  let res = await send(path, init)
  if (res.status === 401 && (await refreshSession())) {
    res = await send(path, init)
  }
  if (!res.ok) {
    const body = await res.json().catch(() => null)
    throw new ApiError(res.status, body?.error ?? 'unknown_error', body?.message ?? res.statusText)
  }
  return (res.status === 204 ? undefined : await res.json()) as T
}

/** Opens a long-lived response, for example the SSE stream. It refreshes the token once on a 401. */
export async function openStream(path: string, signal: AbortSignal): Promise<Response> {
  if (!accessToken) await refreshSession()
  let res = await send(path, { signal })
  if (res.status === 401 && (await refreshSession())) {
    res = await send(path, { signal })
  }
  if (!res.ok) {
    const body = await res.json().catch(() => null)
    throw new ApiError(res.status, body?.error ?? 'unknown_error', body?.message ?? res.statusText)
  }
  return res
}

function send(path: string, init: RequestInit): Promise<Response> {
  const headers = new Headers(init.headers)
  if (accessToken) headers.set('Authorization', `Bearer ${accessToken}`)
  if (init.body) headers.set('Content-Type', 'application/json')
  return fetch(path, { ...init, headers })
}

export const getMe = () => api<User>('/api/me')

export const setTheme = (theme: Theme) =>
  api<void>('/api/me/theme', { method: 'PUT', body: JSON.stringify({ theme }) })

const server = (name: string) => `/api/servers/${encodeURIComponent(name)}`

export const getTemplates = () => api<Template[]>('/api/templates')

export const getServers = () => api<ServerSummary[]>('/api/servers')

export const createServer = (name: string) =>
  api<ServerSummary>('/api/servers', { method: 'POST', body: JSON.stringify({ name }) })

export const getTools = (name: string) => api<ToolSummary[]>(`${server(name)}/tools`)

export const importApi = (name: string, source: ImportApi) =>
  api<ImportedApi[]>(`${server(name)}/apis`, { method: 'POST', body: JSON.stringify(source) })

export const createToken = (name: string, tokenName: string) =>
  api<NewToken>(`${server(name)}/tokens`, { method: 'POST', body: JSON.stringify({ name: tokenName }) })
