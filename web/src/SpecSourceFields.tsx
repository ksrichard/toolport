// The spec source fields. The onboarding wizard and the APIs page use them.

import { readSpecFile, type SpecSource } from './specSource'
import type { Template } from './types/Template'

interface SpecSourceFieldsProps {
  spec: SpecSource
  disabled: boolean
  /** With templates, a third choice imports a template. */
  templates?: Template[]
}

export function SpecSourceFields({ spec, disabled, templates }: SpecSourceFieldsProps) {
  return (
    <>
      <div className="skins" role="group" aria-label="Spec source">
        <button type="button" aria-pressed={spec.mode === 'url'} onClick={() => spec.setMode('url')}>
          From a URL
        </button>
        <button type="button" aria-pressed={spec.mode === 'file'} onClick={() => spec.setMode('file')}>
          From a file
        </button>
        {templates && (
          <button type="button" aria-pressed={spec.mode === 'template'} onClick={() => spec.setMode('template')}>
            From a template
          </button>
        )}
      </div>
      {spec.mode === 'url' && (
        <label className="field">
          <span>Spec URL</span>
          <input
            type="url"
            placeholder="https://petstore3.swagger.io/api/v3/openapi.json"
            value={spec.url}
            onChange={(e) => spec.setUrl(e.target.value)}
            disabled={disabled}
          />
          <small>The daily check can download this URL again and update the tools.</small>
        </label>
      )}
      {spec.mode === 'file' && (
        <label className="field">
          <span>Spec file (JSON)</span>
          <input type="file" accept=".json,application/json" disabled={disabled} onChange={async (e) => spec.setFile(await readSpecFile(e.target))} />
        </label>
      )}
      {spec.mode === 'template' && templates && (
        <label className="field">
          <span>Template</span>
          <select value={spec.template} onChange={(e) => spec.setTemplate(e.target.value)} disabled={disabled}>
            <option value="">Pick a template…</option>
            {templates.map((t) => (
              <option key={t.id} value={t.id}>
                {t.title} ({t.host})
              </option>
            ))}
          </select>
        </label>
      )}
      {spec.mode !== 'template' && (
        <label className="field">
          <span>Base URL (optional)</span>
          <input
            type="url"
            placeholder="The first server URL in the spec"
            value={spec.baseUrl}
            onChange={(e) => spec.setBaseUrl(e.target.value)}
            disabled={disabled}
          />
          <small>ToolPort sends the tool calls to this URL. Only OpenAPI 3.0 JSON specs work for now.</small>
        </label>
      )}
    </>
  )
}
