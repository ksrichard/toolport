// The base URL and auth of an API. The tool builder and the APIs page both edit them.

import { updateApi } from './api'
import type { ApiView } from './types/ApiView'
import type { AuthInput } from './types/AuthInput'

export const AUTH_TYPES = ['none', 'api_key', 'bearer'] as const
export type AuthType = (typeof AUTH_TYPES)[number]

export interface AuthState {
  type: AuthType
  header: string
  secret: string
  /** True when the user wants to type a new secret for a saved one. */
  replacing: boolean
}

export const authOf = (api: ApiView | undefined): AuthState => ({
  type: api?.auth.type ?? 'none',
  header: api?.auth.type === 'api_key' ? api.auth.header : 'X-API-Key',
  secret: '',
  replacing: false,
})

/** The secret is sent only when the user typed one. */
export function authInput(auth: AuthState): AuthInput {
  const secret = auth.secret === '' ? undefined : auth.secret
  if (auth.type === 'api_key') return { type: 'api_key', header: auth.header, secret }
  if (auth.type === 'bearer') return { type: 'bearer', secret }
  return { type: 'none' }
}

export const isApiDirty = (api: ApiView, baseUrl: string, auth: AuthState) =>
  baseUrl !== api.base_url ||
  auth.type !== api.auth.type ||
  (auth.type === 'api_key' && api.auth.type === 'api_key' && auth.header !== api.auth.header) ||
  auth.secret !== ''

/** Saves the base URL and auth. The name stays, because the name of a spec API is fixed. */
export const saveApiEdit = (server: string, api: ApiView, baseUrl: string, auth: AuthState) =>
  updateApi(server, api.id, { name: api.name, base_url: baseUrl.trim(), auth: authInput(auth) })
