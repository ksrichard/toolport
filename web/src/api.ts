// This module keeps the access JWT in memory only. The refresh token is an HttpOnly cookie
// that JavaScript cannot read. The browser sends it only to /auth/*.

import type { ApiView } from './types/ApiView'
import type { CreateServerWithTool } from './types/CreateServerWithTool'
import type { ImportApi } from './types/ImportApi'
import type { ImportedApi } from './types/ImportedApi'
import type { NewToken } from './types/NewToken'
import type { ReimportApi } from './types/ReimportApi'
import type { RemovedApi } from './types/RemovedApi'
import type { SaveApi } from './types/SaveApi'
import type { SaveTool } from './types/SaveTool'
import type { ServerSummary } from './types/ServerSummary'
import type { SetAutoCheck } from './types/SetAutoCheck'
import type { SetToolEnabled } from './types/SetToolEnabled'
import type { Template } from './types/Template'
import type { Theme } from './types/Theme'
import type { TestRequest } from './types/TestRequest'
import type { TestResult } from './types/TestResult'
import type { TokenView } from './types/TokenView'
import type { ToolSummary } from './types/ToolSummary'
import type { ToolView } from './types/ToolView'
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

export const getTokens = (name: string) => api<TokenView[]>(`${server(name)}/tokens`)

export const createToken = (name: string, tokenName: string) =>
  api<NewToken>(`${server(name)}/tokens`, { method: 'POST', body: JSON.stringify({ name: tokenName }) })

export const revokeToken = (name: string, id: number) =>
  api<TokenView>(`${server(name)}/tokens/${id}/revoke`, { method: 'POST' })

const json = (method: string, body: unknown): RequestInit => ({ method, body: JSON.stringify(body) })
const toolPath = (name: string, tool: string) => `${server(name)}/tools/${encodeURIComponent(tool)}`

export const getApis = (name: string) => api<ApiView[]>(`${server(name)}/apis`)

export const createManualApi = (name: string, body: SaveApi) =>
  api<ApiView>(`${server(name)}/apis/manual`, json('POST', body))

export const updateApi = (name: string, id: number, body: SaveApi) =>
  api<ApiView>(`${server(name)}/apis/${id}`, json('PUT', body))

/** Without a spec text, the server downloads the spec URL of the API again. */
export const reimportApi = (name: string, id: number, body: ReimportApi) =>
  api<ImportedApi>(`${server(name)}/apis/${id}/reimport`, json('POST', body))

/** The server refuses when no enabled tool would remain on the server. */
export const removeApi = (name: string, id: number) =>
  api<RemovedApi>(`${server(name)}/apis/${id}`, { method: 'DELETE' })

export const setAutoCheck = (name: string, id: number, enabled: boolean) =>
  api<ApiView>(`${server(name)}/apis/${id}/auto-check`, json('PUT', { enabled } satisfies SetAutoCheck))

export const getTool = (name: string, tool: string) => api<ToolView>(toolPath(name, tool))

export const createTool = (name: string, body: SaveTool) => api<ToolView>(`${server(name)}/tools`, json('POST', body))

export const updateTool = (name: string, tool: string, body: SaveTool) =>
  api<ToolView>(toolPath(name, tool), json('PUT', body))

export const resetTool = (name: string, tool: string) =>
  api<ToolView>(`${toolPath(name, tool)}/reset`, { method: 'POST' })

export const detachTool = (name: string, tool: string) =>
  api<ToolView>(`${toolPath(name, tool)}/detach`, { method: 'POST' })

export const setToolEnabled = (name: string, tool: string, enabled: boolean) =>
  api<void>(`${toolPath(name, tool)}/enabled`, json('PUT', { enabled } satisfies SetToolEnabled))

export const deleteTool = (name: string, tool: string) => api<void>(toolPath(name, tool), { method: 'DELETE' })

export const testTool = (name: string, body: TestRequest) =>
  api<TestResult>(`${server(name)}/tool-test`, json('POST', body))

/** The test for a user without a server yet. The API in the body must be a draft. */
export const testDraftTool = (body: TestRequest) => api<TestResult>('/api/tool-test', json('POST', body))

export const createServerWithTool = (body: CreateServerWithTool) =>
  api<ServerSummary>('/api/servers/with-tool', json('POST', body))
