// Checks that schemaFromRows gives the same schema as the Rust code.
// Both sides read crates/core/tests/fixtures/preview.json.
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { schemaFromRows } from '../src/inputs.ts'

const file = new URL('../../crates/core/tests/fixtures/preview.json', import.meta.url)
const cases = JSON.parse(readFileSync(file, 'utf8'))

for (const c of cases) {
  try {
    assert.deepStrictEqual(schemaFromRows(c.rows, c.body), c.schema)
  } catch (error) {
    console.error(`case failed: ${c.name}`)
    console.error(error)
    process.exit(1)
  }
}
console.log(`${cases.length} cases match`)
