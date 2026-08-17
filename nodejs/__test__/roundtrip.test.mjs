// What the package promises, checked against a real file on disk.

import assert from 'node:assert/strict'
import { mkdtempSync, readFileSync, statSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { test } from 'node:test'

import bizstd from '../lib/index.js'

const scratch = () => mkdtempSync(join(tmpdir(), 'bizstd-'))

const sampleSchema = {
  name: 'samples@1',
  fields: [
    { name: 'timeNanos', ty: 'u64', offset: 0 },
    { name: 'value', ty: 'f64', offset: 8 },
  ],
  fixedSize: 16,
}

function record(timeNanos, value) {
  const buffer = Buffer.alloc(16)
  buffer.writeBigUInt64LE(BigInt(timeNanos), 0)
  buffer.writeDoubleLE(value, 8)
  return buffer
}

test('a full life roundtrip survives reopen', () => {
  const path = join(scratch(), 'day.bizstd')
  const file = bizstd.create(path, sampleSchema, { source: 'test', writer: 'node:test' })
  for (let index = 0; index < 100; index += 1) file.append(record(index, index))
  file.closeFrame(0)
  for (let index = 100; index < 150; index += 1) file.append(record(index, index))
  file.seal(1)

  const back = bizstd.openRead(path)
  assert.equal(back.headers._sealed, 'true')
  assert.equal(back.headers._records, '150')
  assert.equal(back.frames.length, 2)
  assert.equal(back.recordCount, 150n)
  assert.ok(back.frames.every((frame) => frame.hash !== 0n), 'every frame records its checksum')

  const records = [...back]
  assert.equal(records.length, 150)
  assert.equal(records[0].readBigUInt64LE(0), 0n)
  assert.equal(records[149].readBigUInt64LE(0), 149n)
})

test('iteration covers the tail that is still open', () => {
  const path = join(scratch(), 'tail.bizstd')
  const file = bizstd.create(path, sampleSchema, { source: 'test', writer: 'node:test' })
  for (let index = 0; index < 10; index += 1) file.append(record(index, 1))
  file.closeFrame(0)
  for (let index = 10; index < 25; index += 1) file.append(record(index, 2))
  file.flush()

  const back = bizstd.openRead(path)
  assert.equal([...back].length, 25)
  assert.equal([...back.records({ includeTail: false })].length, 10)
})

test('application headers survive', () => {
  const path = join(scratch(), 'headers.bizstd')
  const file = bizstd.create(path, sampleSchema, {
    source: 'test',
    writer: 'node:test',
    headers: { stream: 'alpha', region: 'north' },
  })
  file.append(record(1, 1))
  file.seal(0)

  const back = bizstd.openRead(path)
  assert.equal(back.headers.stream, 'alpha')
  assert.equal(back.headers.region, 'north')
})

test('a record of the wrong size is a usage error', () => {
  const path = join(scratch(), 'wrong.bizstd')
  const file = bizstd.create(path, sampleSchema, { source: 'test', writer: 'node:test' })
  assert.throws(() => file.append(Buffer.from('short')), (error) => {
    assert.ok(error instanceof bizstd.BizstdUsageError, `got ${error.constructor.name}`)
    assert.equal(error.code, 'BIZSTD_USAGE')
    assert.ok(!error.message.startsWith('BIZSTD_'), 'the code is not repeated in the message')
    return true
  })
})

test('a missing file is an io error', () => {
  assert.throws(() => bizstd.openRead(join(scratch(), 'absent.bizstd')), bizstd.BizstdIOError)
})

test('a truncated file is malformed', () => {
  const path = join(scratch(), 'torn.bizstd')
  const file = bizstd.create(path, sampleSchema, { source: 'test', writer: 'node:test' })
  file.append(record(1, 1))
  file.seal(0)
  writeFileSync(path, readFileSync(path).subarray(0, 12))
  assert.throws(() => bizstd.openRead(path), (error) => error instanceof bizstd.BizstdError)
})

test('maintenance agrees with the data', () => {
  const path = join(scratch(), 'maintenance.bizstd')
  const file = bizstd.create(path, sampleSchema, { source: 'test', writer: 'node:test' })
  for (let index = 0; index < 200; index += 1) file.append(record(index, index))
  file.seal(0)

  const report = bizstd.validate(path)
  assert.deepEqual(report.problems, [])
  assert.equal(report.frames, 1)
  assert.equal(report.records, 200n)

  assert.deepEqual(bizstd.rebuildHeaders(path, false).differences, [])

  const before = BigInt(statSync(path).size)
  const repacked = bizstd.repack(path, bizstd.COLD_LEVEL)
  assert.equal(repacked.frames, 1)
  assert.equal(repacked.bytesBefore, before)
  assert.deepEqual(bizstd.validate(path).problems, [])
})

test('prefixed records come back without their prefix', () => {
  const path = join(scratch(), 'prefixed.bizstd')
  const schema = {
    name: 'book@1',
    fields: [{ name: 'nLevels', ty: 'u16', offset: 0 }],
  }
  const file = bizstd.create(path, schema, { source: 'test', writer: 'node:test' })
  file.append(Buffer.from([1, 2, 3]))
  file.append(Buffer.from([4, 5, 6, 7, 8]))
  file.seal(0)

  const back = bizstd.openRead(path)
  const records = [...back]
  assert.deepEqual([...records[0]], [1, 2, 3])
  assert.deepEqual([...records[1]], [4, 5, 6, 7, 8])
})

test('peek reads headers without the data', () => {
  const path = join(scratch(), 'peek.bizstd')
  const file = bizstd.create(path, sampleSchema, { source: 'test', writer: 'node:test' })
  file.append(record(1, 1))
  file.seal(0)

  const { preamble, headers } = bizstd.peekHeaders(path)
  assert.equal(preamble.version, bizstd.VERSION)
  assert.equal(preamble.headerArea, bizstd.DEFAULT_HEADER_AREA)
  assert.equal(headers._schema, 'samples@1')
})

test('the header zone fills up and says which failure it is', () => {
  const path = join(scratch(), 'full.bizstd')
  const file = bizstd.create(path, sampleSchema, { source: 'test', writer: 'node:test' })
  let closed = 0
  assert.throws(
    () => {
      for (; closed < 10_000; closed += 1) {
        file.append(record(closed, 1))
        file.closeFrame(closed)
      }
    },
    (error) => {
      assert.ok(error instanceof bizstd.BizstdZoneFullError, `got ${error.constructor.name}`)
      assert.equal(error.code, 'BIZSTD_ZONE_FULL')
      return true
    },
  )
  assert.ok(BigInt(closed) >= bizstd.maxFramesFor(bizstd.DEFAULT_HEADER_AREA))

  // And repacking with a larger zone is the way out, not a retry.
  bizstd.repack(path, bizstd.HOT_LEVEL, 64 * 1024)
  const wider = bizstd.openAppend(path)
  wider.append(record(1, 1))
  wider.closeFrame(999)
})

test('xxh64 matches the specification vectors', () => {
  assert.equal(bizstd.xxh64(Buffer.from('')), 0xef46db3751d8e999n)
  assert.equal(bizstd.xxh64(Buffer.from('abc')), 0x44bc2cf5ad770999n)
})
