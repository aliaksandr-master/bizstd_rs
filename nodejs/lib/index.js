'use strict'

/**
 * bizstd — an append-only container for records that arrive continuously.
 *
 * The compiled work happens in the native addon. What lives here is the part
 * worth reading: iteration, typed errors, and names that say what they do.
 *
 * One writer per file, and enforcing that is the caller's job: nothing here
 * takes a lock, and two writers on one path corrupt it silently.
 */

const native = require('../index.js')

/** Base of every error this package throws. */
class BizstdError extends Error {
  /**
   * @param {string} message
   * @param {string} code
   */
  constructor(message, code) {
    super(message)
    this.name = new.target.name
    /** @type {string} */
    this.code = code
  }
}

/** The file is not a well-formed container. */
class BizstdMalformedError extends BizstdError {}
/** The calling code asked for something it may not. */
class BizstdUsageError extends BizstdError {}
/** The header zone cannot hold the headers; repack with a larger one. */
class BizstdZoneFullError extends BizstdError {}
/** zstd refused to compress or decompress. */
class BizstdCompressionError extends BizstdError {}
/** The operating system said no. */
class BizstdIOError extends BizstdError {}

/** @type {Record<string, typeof BizstdError>} */
const BY_CODE = {
  BIZSTD_MALFORMED: BizstdMalformedError,
  BIZSTD_USAGE: BizstdUsageError,
  BIZSTD_ZONE_FULL: BizstdZoneFullError,
  BIZSTD_COMPRESSION: BizstdCompressionError,
  BIZSTD_IO: BizstdIOError,
}

/**
 * Turns the addon's error into the class that matches it.
 *
 * The native side has no way to construct a JavaScript class, so it prefixes
 * the message with a stable code and this is where that becomes a type. The
 * prefix is stripped: a caller catching `BizstdZoneFullError` should not also
 * have to read `BIZSTD_ZONE_FULL:` in the message.
 *
 * @param {unknown} error
 * @returns {never}
 */
function rethrow(error) {
  const text = error instanceof Error ? error.message : String(error)
  const match = /^(BIZSTD_[A-Z_]+): ([\s\S]*)$/.exec(text)
  if (match === null) {
    throw error
  }
  const code = /** @type {string} */ (match[1])
  const message = /** @type {string} */ (match[2])
  const Kind = BY_CODE[code] ?? BizstdError
  throw new Kind(message, code)
}

/**
 * Runs the addon and converts whatever it throws.
 *
 * @template T
 * @param {() => T} work
 * @returns {T}
 */
function guarded(work) {
  try {
    return work()
  } catch (error) {
    rethrow(error)
  }
}

/** A container opened for reading. */
class Reader {
  /** @param {import('../index.js').Container} inner */
  constructor(inner) {
    /** @type {import('../index.js').Container} */
    this._inner = inner
  }

  /** Every header, system and application alike. @returns {Record<string, string>} */
  get headers() {
    return this._inner.headers()
  }

  /** The closed frames, in file order. @returns {import('../index.js').Frame[]} */
  get frames() {
    return this._inner.frames()
  }

  /** Records the headers claim, closed frames only. @returns {bigint} */
  get recordCount() {
    return this._inner.recordCount()
  }

  /**
   * The decompressed bytes of the frame at this position.
   *
   * By position rather than by id: ids belong to the writer and writers repeat
   * them — partition by hour and a midnight spill closes under hour 0 after
   * hour 23.
   *
   * @param {number} index
   * @returns {Buffer}
   */
  frame(index) {
    return guarded(() => this._inner.readFrameAt(index))
  }

  /** The decompressed bytes of the first frame carrying this id.
   * @param {bigint} frameId @returns {Buffer} */
  frameById(frameId) {
    return guarded(() => this._inner.readFrame(frameId))
  }

  /** The uncompressed tail, whole records only. @returns {Buffer} */
  tail() {
    return guarded(() => this._inner.readTail())
  }

  /**
   * Every record in the file, frame by frame and then the tail.
   *
   * Yields one record at a time, but decompresses one whole frame at a time:
   * that is how the format is laid out, and pretending otherwise would mean
   * decompressing a frame per record.
   *
   * @param {{ includeTail?: boolean }} [options]
   * @returns {Generator<Buffer, void, undefined>}
   */
  *records(options) {
    const includeTail = options?.includeTail ?? true
    const fixedSize = this._inner.fixedSize() ?? undefined
    const frames = this._inner.frames().length
    for (let index = 0; index < frames; index += 1) {
      const raw = guarded(() => this._inner.readFrameAt(index))
      const [records, leftover] = native.splitRecords(raw, fixedSize)
      if (leftover !== 0) {
        throw new BizstdMalformedError(
          `frame at position ${index}: ${leftover} torn byte(s)`,
          'BIZSTD_MALFORMED',
        )
      }
      yield* records
    }
    if (includeTail) {
      const [records] = native.splitRecords(guarded(() => this._inner.readTail()), fixedSize)
      yield* records
    }
  }

  /** @returns {Generator<Buffer, void, undefined>} */
  [Symbol.iterator]() {
    return this.records()
  }
}

/** A container opened for appending. */
class Writer {
  /** @param {import('../index.js').Container} inner */
  constructor(inner) {
    /** @type {import('../index.js').Container} */
    this._inner = inner
  }

  /** Every header, system and application alike. @returns {Record<string, string>} */
  get headers() {
    return this._inner.headers()
  }

  /** The frames closed so far. @returns {import('../index.js').Frame[]} */
  get frames() {
    return this._inner.frames()
  }

  /** Records appended, the unflushed buffer included. @returns {bigint} */
  get recordCount() {
    return this._inner.recordCount()
  }

  /** Appends one record. @param {Buffer | Uint8Array} body @returns {void} */
  append(body) {
    guarded(() => this._inner.append(Buffer.from(body)))
  }

  /** Appends many records. @param {ReadonlyArray<Buffer | Uint8Array>} bodies @returns {void} */
  extend(bodies) {
    for (const body of bodies) {
      this.append(body)
    }
  }

  /** Writes whatever is buffered, without closing a frame. @returns {void} */
  flush() {
    guarded(() => this._inner.flush())
  }

  /**
   * Compresses the tail into one frame, crash-safe.
   *
   * Throws `BizstdZoneFullError` when the header zone can no longer hold
   * another entry — the way out is `repack` with a larger zone, not a retry.
   *
   * @param {bigint | number} frameId
   * @param {number} [level]
   * @returns {void}
   */
  closeFrame(frameId, level) {
    guarded(() => this._inner.closeFrame(BigInt(frameId), level))
  }

  /** Closes the tail and marks the file finished.
   * @param {bigint | number} frameId @param {number} [level] @returns {void} */
  seal(frameId, level) {
    guarded(() => this._inner.seal(BigInt(frameId), level))
  }

  /** Sets an application header. Keys may not start with `_`.
   * @param {string} key @param {string} value @returns {void} */
  setHeader(key, value) {
    guarded(() => this._inner.setHeader(key, value))
  }
}

/**
 * Creates a file and returns it open for appending.
 *
 * `headerArea` decides how many frames the file can ever hold — see
 * `maxFramesFor`. It cannot be changed later without `repack`, so a long-lived
 * file is worth sizing up front.
 *
 * @param {string} path
 * @param {import('../index.js').Schema} schema
 * @param {{ source: string, writer: string, createdAtMillis?: bigint | number,
 *           headerArea?: number, headers?: Record<string, string> }} options
 * @returns {Writer}
 */
function create(path, schema, options) {
  const inner = guarded(() =>
    native.Container.create(
      path,
      schema,
      options.source,
      options.writer,
      options.createdAtMillis === undefined ? undefined : BigInt(options.createdAtMillis),
      options.headerArea,
      options.headers,
    ),
  )
  return new Writer(inner)
}

/** Opens a file read-only. @param {string} path @returns {Reader} */
function openRead(path) {
  return new Reader(guarded(() => native.Container.openRead(path)))
}

/**
 * Opens a file for appending, recovering it first.
 *
 * A pending seal journal is replayed and a torn record is cut off the tail
 * before the handle is returned.
 *
 * @param {string} path
 * @returns {Writer}
 */
function openAppend(path) {
  return new Writer(guarded(() => native.Container.openAppend(path)))
}

/** @param {string} path @returns {import('../index.js').HeadOnly} */
const peekHeaders = (path) => guarded(() => native.peekHeaders(path))
/** @param {string} path @returns {import('../index.js').ValidateReport} */
const validate = (path) => guarded(() => native.validate(path))
/** @param {string} path @param {boolean} [fix] @returns {import('../index.js').RebuildReport} */
const rebuildHeaders = (path, fix) => guarded(() => native.rebuildHeaders(path, fix))
/** @param {string} path @param {number} [level] @param {number} [headerArea]
 *  @returns {import('../index.js').RepackReport} */
const repack = (path, level, headerArea) => guarded(() => native.repack(path, level, headerArea))

module.exports = {
  BizstdError,
  BizstdMalformedError,
  BizstdUsageError,
  BizstdZoneFullError,
  BizstdCompressionError,
  BizstdIOError,
  Reader,
  Writer,
  create,
  openRead,
  openAppend,
  peekHeaders,
  validate,
  rebuildHeaders,
  repack,
  splitRecords: native.splitRecords,
  xxh64: native.xxh64,
  maxFramesFor: native.maxFramesFor,
  VERSION: native.VERSION,
  EXTENSION: native.EXTENSION,
  DEFAULT_HEADER_AREA: native.DEFAULT_HEADER_AREA,
  MAX_HEADER_AREA: native.MAX_HEADER_AREA,
  HOT_LEVEL: native.HOT_LEVEL,
  COLD_LEVEL: native.COLD_LEVEL,
}
