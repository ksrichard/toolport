// Tools page: every tool of the server, grouped by API, with on/off switches.

import { ToolSwitch } from './ToolSwitch'
import { useToolList, type ToolList } from './toolList'
import type { ToolSummary } from './types/ToolSummary'

interface ToolsProps {
  server: string
  navigate: (path: string) => void
  onChanged: () => void
}

function tag(tool: ToolSummary): string {
  if (tool.source === 'manual') return 'hand-made'
  return tool.edited ? 'edited' : 'spec'
}

export function Tools({ server, navigate, onChanged }: ToolsProps) {
  const list = useToolList(server, onChanged)
  const tools = list.tools ?? []
  const on = tools.filter((t) => t.enabled).length
  const toolUrl = (tool: string) => `/servers/${encodeURIComponent(server)}/tools/${encodeURIComponent(tool)}`

  const groups = new Map<string, ToolSummary[]>()
  for (const tool of tools) {
    groups.set(tool.api, [...(groups.get(tool.api) ?? []), tool])
  }

  return (
    <>
      <div className="head">
        <div>
          <h1>Tools</h1>
          <div className="sub">
            {list.tools ? `${on} of ${tools.length} on. ` : ''}AI clients do not see tools that are off.
          </div>
        </div>
        <div className="head-actions">
          <button className="btn primary" type="button" onClick={() => navigate(toolUrl('new'))}>
            New tool
          </button>
        </div>
      </div>

      {list.loadError && (
        <p className="alert" role="alert">
          Could not load the tools: {list.loadError}
        </p>
      )}
      {list.switchError && (
        <p className="alert" role="alert">
          {list.switchError}
        </p>
      )}

      {[...groups].map(([api, apiTools]) => (
        <section className="panel" key={api}>
          <h2>
            {api} <small>{apiTools.filter((t) => t.enabled).length} of {apiTools.length} on</small>
          </h2>
          <ul className="list tool-rows">
            {apiTools.map((t) => (
              <ToolRow key={t.name} tool={t} list={list} href={toolUrl(t.name)} navigate={navigate} />
            ))}
          </ul>
        </section>
      ))}
    </>
  )
}

interface ToolRowProps {
  tool: ToolSummary
  list: ToolList
  href: string
  navigate: (path: string) => void
}

function ToolRow({ tool, list, href, navigate }: ToolRowProps) {
  return (
    <li>
      <ToolSwitch tool={tool} list={list} />
      <div className="t">
        <div className="tool-name">
          <span className="mono">{tool.name}</span>
          <span className="method">{tag(tool)}</span>
          {tool.spec_changed && <span className="pill warn">spec changed</span>}
        </div>
        <div className="mono muted tool-path">
          {tool.method} {tool.path}
        </div>
        {tool.description && (
          <div className="muted tool-desc" title={tool.description}>
            {tool.description}
          </div>
        )}
      </div>
      <a
        className="btn"
        href={href}
        aria-label={`Edit ${tool.name}`}
        onClick={(e) => {
          e.preventDefault()
          navigate(href)
        }}
      >
        Edit
      </a>
    </li>
  )
}
