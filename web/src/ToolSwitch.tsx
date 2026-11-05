import { usePlan } from './plans'
import type { ToolList } from './toolList'
import type { ToolSummary } from './types/ToolSummary'

/**
 * Why the switch is locked, or null. A paused tool waits for a free place under the plan limit.
 * An off tool can turn on only when the plan has a free place.
 */
function lockReason(tool: ToolSummary, list: ToolList, planName: string | undefined, limit: number | null | undefined) {
  if (tool.paused) return `This tool is paused. The ${planName ?? 'current'} plan serves only ${limit ?? 'some'} tools per server.`
  if (tool.enabled || limit == null || !list.tools) return null
  const served = list.tools.filter((t) => t.enabled && !t.paused).length
  return served >= limit ? `The ${planName ?? 'current'} plan allows ${limit} enabled tools per server. Turn off another tool first.` : null
}

export function ToolSwitch({ tool, list }: { tool: ToolSummary; list: ToolList }) {
  const plan = usePlan().billing?.plan
  const locked = lockReason(tool, list, plan?.name, plan?.max_tools_per_server)
  return (
    <label className={locked ? 'toggle is-locked' : 'toggle'} title={locked ?? undefined}>
      <input
        type="checkbox"
        role="switch"
        checked={tool.enabled && !tool.paused}
        disabled={locked !== null || list.pending.has(tool.name)}
        onChange={(e) => list.setEnabled(tool.name, e.target.checked)}
        aria-label={tool.name}
        aria-description={locked ?? undefined}
      />
      <span />
    </label>
  )
}
