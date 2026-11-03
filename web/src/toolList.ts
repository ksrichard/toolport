// The tool list of one server and its on/off switches. The Live page and the Tools page use it.

import { useEffect, useState } from 'react'
import { getTools, setToolEnabled } from './api'
import type { ToolSummary } from './types/ToolSummary'

export interface ToolList {
  tools: ToolSummary[] | null
  loadError: string | null
  /** The message of the last failed switch change. */
  switchError: string | null
  /** Names of the tools with a request that runs now. */
  pending: ReadonlySet<string>
  setEnabled: (tool: string, enabled: boolean) => void
}

/** `onChanged` runs after the server accepts a change, for example to update the menu counts. */
export function useToolList(server: string, onChanged?: () => void): ToolList {
  const [tools, setTools] = useState<ToolSummary[] | null>(null)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [switchError, setSwitchError] = useState<string | null>(null)
  const [pending, setPending] = useState<ReadonlySet<string>>(new Set())

  useEffect(() => {
    let cancelled = false
    getTools(server).then(
      (list) => !cancelled && setTools(list),
      (e: Error) => !cancelled && setLoadError(e.message),
    )
    return () => {
      cancelled = true
    }
  }, [server])

  const update = (tool: string, enabled: boolean) =>
    setTools((list) => list?.map((t) => (t.name === tool ? { ...t, enabled } : t)) ?? null)
  const setPendingFor = (tool: string, on: boolean) =>
    setPending((set) => {
      const next = new Set(set)
      if (on) next.add(tool)
      else next.delete(tool)
      return next
    })

  const setEnabled = (tool: string, enabled: boolean) => {
    // The switch shows the new state at once and goes back if the server refuses it.
    update(tool, enabled)
    setPendingFor(tool, true)
    setSwitchError(null)
    setToolEnabled(server, tool, enabled).then(
      () => {
        setPendingFor(tool, false)
        onChanged?.()
      },
      (e: Error) => {
        update(tool, !enabled)
        setPendingFor(tool, false)
        setSwitchError(e.message)
      },
    )
  }

  return { tools, loadError, switchError, pending, setEnabled }
}
