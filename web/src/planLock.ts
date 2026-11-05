// The plan rules for the tool switches. The switch and the tool rows share them.

import { usePlan } from './plans'
import type { ToolList } from './toolList'
import type { ToolSummary } from './types/ToolSummary'

/**
 * Why the switch is locked, or null. A paused or off tool can turn on only when the plan has a
 * free place on the server. A served tool can always turn off.
 */
export function lockReason(tool: ToolSummary, list: ToolList, planName: string | undefined, limit: number | null | undefined) {
  const served = tool.enabled && !tool.paused
  if (served || limit == null || !list.tools) return null
  const used = list.tools.filter((t) => t.enabled && !t.paused).length
  if (used < limit) return null
  const plan = planName ?? 'current'
  return tool.paused
    ? `This tool is paused. The ${plan} plan allows ${limit} enabled tools per server. Turn off another tool first.`
    : `The ${plan} plan allows ${limit} enabled tools per server. Turn off another tool first.`
}

/** True when the plan keeps the tool off: it is paused, or it is off and the server has no free place. */
export function usePlanBlocked(tool: ToolSummary, list: ToolList): boolean {
  const plan = usePlan().billing?.plan
  return tool.paused || lockReason(tool, list, plan?.name, plan?.max_tools_per_server) !== null
}
