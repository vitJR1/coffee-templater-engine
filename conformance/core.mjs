// A minimal JavaScript host for coffee_core.wasm: frames, memory management (ct_alloc/ct_free)
// and the ct_host.call import. Shared by run-wasm.mjs and trap-recovery.mjs.

// ------------------------------------------------------------------------------- frames

const enc = new TextEncoder()
const dec = new TextDecoder()

export function encodeFrame(json, blobs = []) {
  const j = enc.encode(typeof json === 'string' ? json : JSON.stringify(json))
  const size = 12 + j.length + blobs.reduce((n, b) => n + 4 + b.length, 0)
  const out = new Uint8Array(size)
  const dv = new DataView(out.buffer)
  out.set(enc.encode('CTF1'), 0)
  dv.setUint32(4, j.length, true)
  out.set(j, 8)
  let p = 8 + j.length
  dv.setUint32(p, blobs.length, true)
  p += 4
  for (const b of blobs) {
    dv.setUint32(p, b.length, true)
    out.set(b, p + 4)
    p += 4 + b.length
  }
  return out
}

export function decodeFrame(buf) {
  const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength)
  if (dec.decode(buf.subarray(0, 4)) !== 'CTF1') throw new Error('bad frame magic')
  const jl = dv.getUint32(4, true)
  const json = JSON.parse(dec.decode(buf.subarray(8, 8 + jl)))
  let p = 8 + jl
  const n = dv.getUint32(p, true)
  p += 4
  const blobs = []
  for (let i = 0; i < n; i++) {
    const l = dv.getUint32(p, true)
    blobs.push(buf.slice(p + 4, p + 4 + l))
    p += 4 + l
  }
  return { json, blobs }
}

// ------------------------------------------------------------------------------- the core

export class Core {
  constructor(module, onCall = () => null) {
    const self = this
    this.instance = new WebAssembly.Instance(module, {
      ct_host: {
        call(ptr, len) {
          const req = decodeFrame(self.bytes(ptr, len).slice())
          const res = onCall(req.json, req.blobs)
          if (!res) return 0
          const frame = encodeFrame(res.json, res.blobs)
          // [u32 len][frame], allocated with ct_alloc; the core frees it
          const out = self.ex.ct_alloc(frame.length + 4)
          new DataView(self.ex.memory.buffer).setUint32(out, frame.length, true)
          self.bytes(out + 4, frame.length).set(frame)
          return out
        },
      },
    })
    this.ex = this.instance.exports
    if (this.ex.ct_abi_version() !== 1) throw new Error('unsupported ABI')
  }

  bytes(ptr, len) {
    return new Uint8Array(this.ex.memory.buffer, ptr, len)
  }

  call(json, blobs = []) {
    const req = encodeFrame(json, blobs)
    const ptr = this.ex.ct_alloc(req.length)
    this.bytes(ptr, req.length).set(req)
    const res = this.ex.ct_call(ptr, req.length)
    this.ex.ct_free(ptr, req.length)
    const len = new DataView(this.ex.memory.buffer).getUint32(res, true)
    const frame = this.bytes(res + 4, len).slice()
    this.ex.ct_free(res, len + 4)
    return decodeFrame(frame)
  }
}
