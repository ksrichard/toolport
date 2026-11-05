// The plans come from GET /api/plans, so a plan change in the database needs no frontend change.

import { createContext, useContext, useEffect, useState } from 'react'
import type { Billing } from './types/Billing'
import type { Plan } from './types/Plan'

export type { Billing, Plan }

// The list changes rarely, so the page loads it once.
let cached: Promise<Plan[]> | null = null

/** The route is public, so the landing page can call it without a token. */
function loadPlans(): Promise<Plan[]> {
  cached ??= fetch('/api/plans')
    .then((res) => {
      if (!res.ok) throw new Error(`The plans did not load (HTTP ${res.status}).`)
      return res.json() as Promise<Plan[]>
    })
    .catch((e: unknown) => {
      cached = null
      throw e
    })
  return cached
}

/** Returns null while the plans load. `error` is set when the load fails. */
export function usePlans(): { plans: Plan[] | null; error: string | null } {
  const [plans, setPlans] = useState<Plan[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  useEffect(() => {
    let active = true
    loadPlans().then(
      (list) => active && setPlans(list),
      (e: Error) => active && setError(e.message),
    )
    return () => {
      active = false
    }
  }, [])
  return { plans, error }
}

/** The plan and usage of the signed-in user. The shell loads it once and shares it. */
export interface PlanState {
  billing: Billing | null
  /** Loads the plan again, for example after an action that changes the usage. */
  reload: () => void
}

export const PlanContext = createContext<PlanState>({ billing: null, reload: () => {} })

export const usePlan = () => useContext(PlanContext)

const count = new Intl.NumberFormat('en-US')

export const UNLIMITED = 'Unlimited'

export function limitText(value: number | null): string {
  return value === null ? UNLIMITED : count.format(value)
}

export function priceText(plan: Plan): string {
  const money = new Intl.NumberFormat('en-US', {
    style: 'currency',
    currency: plan.currency.toUpperCase(),
    minimumFractionDigits: plan.price_cents % 100 === 0 ? 0 : 2,
  }).format(plan.price_cents / 100)
  return plan.interval ? `${money} / ${plan.interval}` : money
}

export function historyText(hours: number): string {
  if (hours % 24 !== 0 || hours < 48) return `${hours} hours`
  return `${hours / 24} days`
}

/** For example `1 server` or `10 servers`. */
export function plural(n: number, word: string): string {
  return `${count.format(n)} ${word}${n === 1 ? '' : 's'}`
}

/** The day in UTC, for example `Dec 1, 2026`. Plan periods are in UTC. */
export function dayText(ms: number): string {
  return new Date(ms).toLocaleDateString('en-US', { month: 'short', day: 'numeric', year: 'numeric', timeZone: 'UTC' })
}

/** Part of the monthly quota that is used, from 0 to 1. 0 when the plan has no call limit. */
export function quotaShare(billing: Billing): number {
  const max = billing.plan.max_calls_per_month
  if (max === null) return 0
  if (max === 0) return 1
  return Math.min(1, billing.calls_used / max)
}

/** The tone of the usage bar and the banner: warn at 80 %, bad at 100 %. */
export function quotaTone(billing: Billing): 'ok' | 'warn' | 'bad' {
  const share = quotaShare(billing)
  if (share >= 1) return 'bad'
  if (share >= 0.8) return 'warn'
  return 'ok'
}
