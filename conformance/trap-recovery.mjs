// A host exception thrown from a callback unwinds through the module (a trap). The next ct_call
// must report it once and then work normally on a fresh instance.
//
//     node conformance/trap-recovery.mjs [path/to/coffee_core.wasm]

import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const ROOT = dirname(fileURLToPath(import.meta.url))
const wasm = readFileSync(process.argv[2] ?? join(ROOT, '..', 'dist', 'coffee_core.wasm'))
const enc = new TextEncoder()
const dec = new TextDecoder()

let ex
const { instance } = await WebAssembly.instantiate(wasm, { ct_host: { call: () => { throw new Error('host exploded') } } })
ex = instance.exports

function frame(json, blobs = []) {
  const j = enc.encode(JSON.stringify(json))
  const out = new Uint8Array(12 + j.length + blobs.reduce((n, b) => n + 4 + b.length, 0))
  const dv = new DataView(out.buffer)
  out.set(enc.encode('CTF1'))
  dv.setUint32(4, j.length, true)
  out.set(j, 8)
  let p = 8 + j.length
  dv.setUint32(p, blobs.length, true)
  p += 4
  for (const b of blobs) { dv.setUint32(p, b.length, true); out.set(b, p + 4); p += 4 + b.length }
  return out
}

function call(json, blobs) {
  const req = frame(json, blobs)
  const ptr = ex.ct_alloc(req.length)
  new Uint8Array(ex.memory.buffer, ptr, req.length).set(req)
  const res = ex.ct_call(ptr, req.length)
  const len = new DataView(ex.memory.buffer).getUint32(res, true)
  const f = new Uint8Array(ex.memory.buffer, res + 4, len).slice()
  ex.ct_free(res, len + 4)
  const jl = new DataView(f.buffer).getUint32(4, true)
  return JSON.parse(dec.decode(f.subarray(8, 8 + jl)))
}

const tpl = readFileSync(join(ROOT, 'fixtures', 'invoice.docx'))
let threw = false
try {
  call({ op: 'renderOnce', template: { $blob: 0 }, options: { hostFilters: ['number'] }, data: { items: [{ price: 1 }] } }, [tpl])
} catch (e) {
  threw = e.message === 'host exploded'
}
const after = call({ op: 'info' })
const again = call({ op: 'info' })
const ok = threw && after.ok === false && again.ok === true && again.result.abiVersion === 1
console.log(ok ? 'ok    trap recovery' : `FAIL  trap recovery: ${JSON.stringify({ threw, after, again })}`)
process.exit(ok ? 0 : 1)
