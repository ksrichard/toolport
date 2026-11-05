import { lockReason } from './planLock'
import { usePlan } from './plans'
import type { ToolList } from './toolList'
import type { ToolSummary } from './types/ToolSummary'

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
