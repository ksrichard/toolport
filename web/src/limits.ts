// The server rate limits for MCP tokens. The pages show them as plain info text.

import { useEffect, useState } from 'react'
import { api } from './api'
import type { Limits } from './types/Limits'

// The value changes only when the server restarts, so the app loads it once.
let cached: Promise<Limits> | null = null

function loadLimits(): Promise<Limits> {
  cached ??= api<Limits>('/api/limits').catch((e: unknown) => {
    cached = null
    throw e
  })
  return cached
}

/** Returns null while the value loads or when the load fails. The info text then stays hidden. */
export function useLimits(): Limits | null {
  const [limits, setLimits] = useState<Limits | null>(null)
  useEffect(() => {
    let active = true
    loadLimits().then(
      (value) => active && setLimits(value),
      () => {},
    )
    return () => {
      active = false
    }
  }, [])
  return limits
}

const calls = (n: number) => (n === 1 ? '1 tool call' : `${n} tool calls`)

/** The full rule for the Tokens page. */
export function rateLimitRule(limits: Limits): string {
  const perToken = limits.rate_limit_per_minute
  const perUser = limits.user_rate_limit_per_minute
  if (perToken === 0 && perUser === 0) return 'Tokens have no rate limit.'
  if (perUser === 0) {
    return (
      `Each token can make ${calls(perToken)} per minute, with bursts of up to ${limits.rate_limit_burst}. ` +
      'A call over the limit gets an error and does not reach the API.'
    )
  }
  if (perToken === 0) {
    return (
      `All your tokens on all servers share a limit of ${calls(perUser)} per minute, ` +
      `with bursts of up to ${limits.user_rate_limit_burst}. ` +
      'A call over the limit gets an error and does not reach the API.'
    )
  }
  return (
    `Each token can make ${calls(perToken)} per minute, with bursts of up to ${limits.rate_limit_burst}. ` +
    `All your tokens on all servers share a limit of ${calls(perUser)} per minute. ` +
    'A call over a limit gets an error and does not reach the API.'
  )
}

/** One short sentence for the Connect page. It is empty when both limits are off. */
export function rateLimitShort(limits: Limits): string {
  const perToken = limits.rate_limit_per_minute
  const perUser = limits.user_rate_limit_per_minute
  if (perToken === 0 && perUser === 0) return ''
  if (perUser === 0) return `This token can make ${calls(perToken)} per minute.`
  if (perToken === 0) return `All your tokens share a limit of ${calls(perUser)} per minute.`
  return `This token can make ${calls(perToken)} per minute, and all your tokens share ${perUser} per minute.`
}
