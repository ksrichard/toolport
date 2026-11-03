// Builds the tool JSON in the browser. The rules match crates/core/src/inputs.rs.
// scripts/check-preview.ts checks that both sides give the same schema.
// Keep this file plain TypeScript: Node runs it without a build step.

import type { InputRow } from './types/InputRow'
import type { InputType } from './types/InputType'

type Schema = Record<string, unknown>

export const BODY_ARGUMENT = 'body'

const TYPE_NAMES: Record<Exclude<InputType, 'complex'>, string> = {
  string: 'string',
  number: 'number',
  integer: 'integer',
  boolean: 'boolean',
  string_array: 'array',
}

function propertyOf(row: InputRow): Schema {
  const property: Schema = {}
  if (row.input_type !== 'complex') {
    property.type = TYPE_NAMES[row.input_type]
    if (row.input_type === 'string_array') property.items = { type: 'string' }
  }
  if (row.description !== '') property.description = row.description
  return property
}

/** The input schema of a hand-made tool. */
export function schemaFromRows(rows: InputRow[], body?: unknown): Schema {
  const properties: Record<string, unknown> = {}
  for (const row of rows) properties[row.name] = propertyOf(row)
  if (body !== undefined && body !== null) properties[BODY_ARGUMENT] = body
  const schema: Schema = { type: 'object', properties }
  const required = rows.filter((row) => row.required).map((row) => row.name)
  if (required.length > 0) schema.required = required
  return schema
}

/** The tool as `tools/list` sends it. */
export function toolJson(name: string, description: string, schema: Schema): Schema {
  return { name, description, inputSchema: schema }
}

/** Parses `latitude={lat}&longitude={lon}` into pairs of query key and input name. */
export function parseQueryTemplate(text: string): [string, string][] | Error {
  if (text === '') return []
  const pairs: [string, string][] = []
  for (const part of text.split('&')) {
    const bad = new Error(`the part "${part}" must look like key={input}`)
    const at = part.indexOf('=')
    if (at < 0) return bad
    const key = part.slice(0, at)
    const value = part.slice(at + 1)
    if (!value.startsWith('{') || !value.endsWith('}')) return bad
    const input = value.slice(1, -1)
    const invalid = (s: string) => s === '' || /[{}&=]/.test(s)
    if (invalid(key) || invalid(input)) return bad
    pairs.push([key, input])
  }
  return pairs
}

export function printQueryTemplate(pairs: [string, string][]): string {
  return pairs.map(([key, input]) => `${key}={${input}}`).join('&')
}

/** Names between braces in a path, such as `{id}` in `/pets/{id}`. */
export function pathPlaceholders(path: string): string[] {
  return [...path.matchAll(/\{([^{}]*)\}/g)].map((m) => m[1])
}
