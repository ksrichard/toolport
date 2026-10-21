// The access JWT lives only in this module's memory. The refresh token is an HttpOnly cookie
// that JavaScript cannot read. The browser sends it only to /auth/*.

export type Theme = 'neon' | 'calm' | 'light'

export interface User {
  id: number
  github_login: string
  name: string | null
  avatar_url: string | null
  theme: Theme
}

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

function send(path: string, init: RequestInit): Promise<Response> {
  const headers = new Headers(init.headers)
  if (accessToken) headers.set('Authorization', `Bearer ${accessToken}`)
  if (init.body) headers.set('Content-Type', 'application/json')
  return fetch(path, { ...init, headers })
}

export const getMe = () => api<User>('/api/me')

export const setTheme = (theme: Theme) =>
  api<void>('/api/me/theme', { method: 'PUT', body: JSON.stringify({ theme }) })
