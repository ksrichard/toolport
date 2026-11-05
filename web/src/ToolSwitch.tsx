import { usePlan } from './plans'
import type { ToolList } from './toolList'
import type { ToolSummary } from './types/ToolSummary'

/**
 * Why the switch is locked, or null. A paused or off tool can turn on only when the plan has a
 * free place on the server. A served tool can always turn off.
 */
function lockReason(tool: ToolSummary, list: ToolList, planName: string | undefined, limit: number | null | undefined) {
  const served = tool.enabled && !tool.paused
  if (served || limit == null || !list.tools) return null
  const used = list.tools.filter((t) => t.enabled && !t.paused).length
  if (used < limit) return null
  const plan = planName ?? 'current'
  return tool.paused
    ? `This tool is paused. The ${plan} plan allows ${limit} enabled tools per server. Turn off another tool first.`
    : `The ${plan} plan allows ${limit} enabled tools per server. Turn off another tool first.`
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
