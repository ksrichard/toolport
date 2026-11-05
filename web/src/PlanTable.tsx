// The plan cards with a row-label column. The landing page and the Billing page use it.
// The rows are fixed here. Their values come from GET /api/plans.

import type { CSSProperties, ReactNode } from 'react'
import { historyText, limitText, priceText, type Plan } from './plans'

const ROWS: { label: string; value: (plan: Plan) => string }[] = [
  { label: 'Price', value: priceText },
  { label: 'MCP servers', value: (p) => limitText(p.max_servers) },
  { label: 'Tools per server', value: (p) => limitText(p.max_tools_per_server) },
  { label: 'Tool calls per month', value: (p) => limitText(p.max_calls_per_month) },
  { label: 'Call history', value: (p) => historyText(p.history_hours) },
]

interface PlanTableProps {
  plans: Plan[]
  /** The code of the plan of the signed-in user. Its card says "Current plan". */
  current?: string
  /** The button or note at the end of a card. */
  action?: (plan: Plan) => ReactNode
}

export function PlanTable({ plans, current, action }: PlanTableProps) {
  return (
    <div className="plan-table" style={{ '--plans': plans.length } as CSSProperties}>
      <div className="pt-labels" aria-hidden="true">
        <div className="pt-head pt-kicker">Per account</div>
        {ROWS.map((row) => (
          <div key={row.label} className="pt-row">
            {row.label}
          </div>
        ))}
      </div>
      {plans.map((plan) => {
        const isCurrent = plan.code === current
        return (
          <article
            key={plan.code}
            className={`panel pt-card${plan.is_default ? '' : ' featured'}${isCurrent ? ' current' : ''}`}
            aria-label={`${plan.name} plan`}
          >
            <div className="pt-head">
              <h3>{plan.name}</h3>
              {isCurrent && <span className="pt-current">Current plan</span>}
            </div>
            <dl>
              {ROWS.map((row) => (
                <div key={row.label} className="pt-row">
                  <dt>{row.label}</dt>
                  <dd>{row.value(plan)}</dd>
                </div>
              ))}
            </dl>
            {action && <div className="pt-action">{action(plan)}</div>}
          </article>
        )
      })}
    </div>
  )
}
