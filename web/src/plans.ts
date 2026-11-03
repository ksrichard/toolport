// The plan limits. The landing page shows them. The app does not apply them yet.
export interface Plan {
  id: 'free' | 'pro'
  name: string
  /** Price in euro per month. */
  priceEur: number
  /** null means no limit. */
  servers: number | null
  tools: number | null
  callsPerMonth: number | null
}

export const PLANS: readonly Plan[] = [
  { id: 'free', name: 'Free', priceEur: 0, servers: 1, tools: 20, callsPerMonth: 1_000 },
  { id: 'pro', name: 'Pro', priceEur: 9, servers: 10, tools: null, callsPerMonth: 100_000 },
]
