// A trap (a host exception thrown from a callback, a panic) cannot be cleaned up inside the
// module, so the instance is poisoned: every later ct_call answers `internal`, and a new instance
// works normally. A ct_call made from inside a callback poisons the instance too.
//
//     node conformance/trap-recovery.mjs [path/to/coffee_core.wasm]

import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { Core } from './core.mjs'

const ROOT = dirname(fileURLToPath(import.meta.url))
const module = new WebAssembly.Module(readFileSync(process.argv[2] ?? join(ROOT, '..', 'dist', 'coffee_core.wasm')))
const tpl = readFileSync(join(ROOT, 'fixtures', 'invoice.docx'))
const render = (core) =>
  core.call({ op: 'renderOnce', template: { $blob: 0 }, options: { hostFilters: ['number'] }, data: { items: [{ price: 1 }] } }, [tpl])
const code = (res) => res.json.errors?.[0]?.code

let failed = 0
function check(name, ok, info) {
  if (!ok) failed++
  console.log(ok ? `ok    ${name}` : `FAIL  ${name}: ${JSON.stringify(info)}`)
}

// 1. a host exception unwinds through the module
{
  const core = new Core(module, () => {
    throw new Error('host exploded')
  })
  let threw = false
  try {
    render(core)
  } catch (e) {
    threw = e.message === 'host exploded'
  }
  const after = [0, 1, 2].map(() => code(core.call({ op: 'info' })))
  check('trap poisons the instance', threw && after.every((c) => c === 'internal'), { threw, after })
}

// 2. many traps on fresh instances: no state leaks between instances
{
  let ok = true
  for (let i = 0; i < 300 && ok; i++) {
    const core = new Core(module, () => {
      throw new Error('boom')
    })
    try {
      render(core)
      ok = false
    } catch {}
    ok &&= code(core.call({ op: 'info' })) === 'internal'
  }
  const fresh = new Core(module).call({ op: 'info' }).json
  check('a new instance works after a trap', ok && fresh.ok === true && fresh.result.abiVersion === 1, { ok, fresh })
}

// 3. ct_call from inside a callback
{
  let inner
  const core = new Core(module, () => {
    inner = code(core.call({ op: 'info' }))
    return { json: { ok: true, value: 'x' } }
  })
  const outer = code(render(core))
  const later = code(core.call({ op: 'info' }))
  check('re-entrant ct_call poisons the instance', inner === 'internal' && outer === 'internal' && later === 'internal', { inner, outer, later })
}

process.exit(failed ? 1 : 0)
