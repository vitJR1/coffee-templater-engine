// Runs the conformance cases against dist/coffee_core.wasm from Node.js — a minimal JS host:
// frames, memory management (ct_alloc/ct_free), the ct_host.call import and declarative host
// filters. sha256 values must match the ones produced by the Rust runner (spec §11–12).
//
//     node conformance/run-wasm.mjs [path/to/coffee_core.wasm]

import { createHash } from 'node:crypto'
import { existsSync, readdirSync, readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { inflateRawSync } from 'node:zlib'
import { Core } from './core.mjs'

const ROOT = dirname(fileURLToPath(import.meta.url))
const WASM = process.argv[2] ?? join(ROOT, '..', 'dist', 'coffee_core.wasm')

const module = new WebAssembly.Module(readFileSync(WASM))
const dec = new TextDecoder()

// ------------------------------------------------------------------------------- helpers

/** Minimal zip reader (stored + deflate) to inspect results. */
function unzip(buf) {
  const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength)
  let eocd = buf.length - 22
  while (dv.getUint32(eocd, true) !== 0x06054b50) eocd--
  const count = dv.getUint16(eocd + 10, true)
  let p = dv.getUint32(eocd + 16, true)
  const files = new Map()
  for (let i = 0; i < count; i++) {
    const method = dv.getUint16(p + 10, true)
    const csize = dv.getUint32(p + 20, true)
    const nlen = dv.getUint16(p + 28, true)
    const elen = dv.getUint16(p + 30, true)
    const clen = dv.getUint16(p + 32, true)
    const local = dv.getUint32(p + 42, true)
    const name = dec.decode(buf.subarray(p + 46, p + 46 + nlen))
    const start = local + 30 + dv.getUint16(local + 26, true) + dv.getUint16(local + 28, true)
    const raw = buf.subarray(start, start + csize)
    files.set(name, method === 8 ? inflateRawSync(raw) : raw)
    p += 46 + nlen + elen + clen
  }
  return files
}

const jsString = (v) => (typeof v === 'string' ? v : typeof v === 'number' ? String(v) : JSON.stringify(v))

function at(v, path) {
  for (const seg of path.split('.').filter(Boolean)) {
    const m = /^([^[]*)((?:\[\d+\])*)$/.exec(seg)
    if (m[1]) v = v?.[m[1]]
    for (const i of m[2].matchAll(/\[(\d+)\]/g)) v = v?.[Number(i[1])]
  }
  return v
}

function matches(actual, expected) {
  if (Array.isArray(expected)) return Array.isArray(actual) && actual.length === expected.length && expected.every((e, i) => matches(actual[i], e))
  if (expected && typeof expected === 'object') return actual && typeof actual === 'object' && Object.entries(expected).every(([k, e]) => matches(actual[k], e))
  return actual === expected
}

function assert(cond, msg) {
  if (!cond) throw new Error(msg)
}

function checkPaths(root, checks, what) {
  for (const c of Array.isArray(checks) ? checks : [checks]) {
    const actual = at(root, c.path)
    assert(matches(actual, c.equals), `${what} ${c.path}: expected ${JSON.stringify(c.equals)}, got ${JSON.stringify(actual)}`)
  }
}

// ------------------------------------------------------------------------------- runner

function hostFor(filters) {
  return (req) => {
    const spec = filters[req.name] ?? {}
    if (spec.type === 'format') {
      let s = spec.pattern.replaceAll('{value}', jsString(req.value))
      req.args.forEach((a, i) => (s = s.replaceAll(`{arg${i}}`, jsString(a))))
      return { json: { ok: true, value: s } }
    }
    if (spec.type === 'upper') return { json: { ok: true, value: jsString(req.value).toUpperCase() } }
    if (spec.type === 'throw') return { json: { ok: false, message: spec.message } }
    return { json: { ok: false, message: 'unknown host filter' } }
  }
}

function runCase(dir, name) {
  const c = JSON.parse(readFileSync(join(dir, 'case.json'), 'utf8'))
  const expect = c.expect
  const core = new Core(module, hostFor(c.hostFilters ?? {}))
  const template = c.template ? readFileSync(join(ROOT, c.template)) : undefined
  const data = existsSync(join(dir, 'data.json')) ? readFileSync(join(dir, 'data.json'), 'utf8') : JSON.stringify(c.data ?? {})
  const blobDir = join(dir, 'blobs')
  const blobs = existsSync(blobDir) ? readdirSync(blobDir).sort().map((f) => readFileSync(join(blobDir, f))) : []
  const options = { ...(c.options ?? {}), ...(c.hostFilters ? { hostFilters: Object.keys(c.hostFilters) } : {}) }

  let res
  if (c.op === 'info') res = core.call({ op: 'info' })
  else if (['compile', 'render', 'inspect'].includes(c.op)) {
    res = core.call({ op: 'compile', template: { $blob: 0 }, options }, [template])
    if (c.op !== 'compile' && res.json.ok) {
      const handle = res.json.result.template
      // data is passed as raw JSON text so number lexemes reach the core unchanged
      res = c.op === 'render' ? core.call(`{"op":"render","template":${handle},"data":${data}}`, blobs) : core.call({ op: 'inspect', template: handle })
      core.call({ op: 'dispose', template: handle })
    }
  } else res = core.call({ op: c.op, document: { $blob: 0 }, ...(c.limits ? { limits: c.limits } : {}) }, [template])

  const r = res.json
  if (expect.errors) {
    assert(r.ok === false, `expected errors, got ${JSON.stringify(r)}`)
    expect.errors.forEach((e, i) => assert(matches(r.errors[i], e), `error ${i}: expected ${JSON.stringify(e)}, got ${JSON.stringify(r.errors[i])}`))
    return
  }
  assert(r.ok === true, JSON.stringify(r))
  if (expect.result) checkPaths(r.result, expect.result, 'result')
  if (c.op === 'extractText') return expect.text === undefined || assert(r.result.text === expect.text, 'text')
  if (c.op === 'parse') return expect.model && checkPaths(r.result.model, expect.model, 'model')
  if (c.op !== 'render') return

  const doc = res.blobs[0]
  const text = () => new Core(module).call({ op: 'extractText', document: { $blob: 0 } }, [doc]).json.result.text
  if (expect.text !== undefined) assert(text() === expect.text, `text:\n${text()}`)
  for (const s of expect.textContains ?? []) assert(text().includes(s), `text does not contain ${JSON.stringify(s)}`)
  if (expect.model) checkPaths(new Core(module).call({ op: 'parse', document: { $blob: 0 } }, [doc]).json.result.model, expect.model, 'model')
  const files = unzip(doc)
  for (const [part, chk] of Object.entries(expect.parts ?? {})) {
    const xml = dec.decode(files.get(part))
    for (const s of chk.contains ?? []) assert(xml.includes(s), `${part} does not contain ${s}`)
    for (const s of chk.notContains ?? []) assert(!xml.includes(s), `${part} contains ${s}`)
  }
  for (const f of expect.files?.contains ?? []) assert(files.has(f), `missing ${f}`)
  for (const f of expect.files?.notContains ?? []) assert(!files.has(f), `unexpected ${f}`)
  if (expect.sha256) {
    const sha = createHash('sha256').update(doc).digest('hex')
    assert(sha === expect.sha256, `sha256 ${sha} != ${expect.sha256}`)
  }
}

let failed = 0
const cases = readdirSync(join(ROOT, 'cases')).sort()
for (const name of cases) {
  try {
    runCase(join(ROOT, 'cases', name), name)
    console.log(`ok    ${name}`)
  } catch (e) {
    failed++
    console.log(`FAIL  ${name}: ${e.message}`)
  }
}
console.log(`\n${cases.length - failed}/${cases.length} cases passed (${WASM})`)
process.exit(failed ? 1 : 0)
