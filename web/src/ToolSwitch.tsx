import type { ToolList } from './toolList'
import type { ToolSummary } from './types/ToolSummary'

export function ToolSwitch({ tool, list }: { tool: ToolSummary; list: ToolList }) {
  return (
    <label className="toggle">
      <input
        type="checkbox"
        role="switch"
        checked={tool.enabled}
        disabled={list.pending.has(tool.name)}
        onChange={(e) => list.setEnabled(tool.name, e.target.checked)}
        aria-label={tool.name}
      />
      <span />
    </label>
  )
}
