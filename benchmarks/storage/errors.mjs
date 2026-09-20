/** Errors and bounded operations used by the storage benchmark runner. */

export class BenchmarkTimeoutError extends Error {
  constructor(operation, timeoutMs) {
    super(`${operation} timed out after ${timeoutMs} ms`)
    this.name = "BenchmarkTimeoutError"
    this.code = "BENCHMARK_TIMEOUT"
    this.operation = operation
    this.timeoutMs = timeoutMs
  }
}

/**
 * Bound an operation without losing the rejection handler on a late promise.
 * Native filesystem calls cannot be cancelled portably, so a timed-out call is
 * still observed by `lateOperation`; callers must not clean its path until that
 * promise settles.
 */
export function withTimeout(operation, timeoutMs, operationName = "operation") {
  if (!Number.isInteger(timeoutMs) || timeoutMs <= 0) {
    return Promise.reject(new RangeError(`timeout must be a positive integer: ${timeoutMs}`))
  }

  const started = Promise.resolve().then(() =>
    typeof operation === "function" ? operation() : operation,
  )
  // Always resolve the observer so a late native rejection is handled and can
  // be inspected without creating an unhandled rejection.
  const lateOperation = started.then(
    (value) => ({ status: "fulfilled", value }),
    (error) => ({ status: "rejected", error }),
  )

  return new Promise((resolve, reject) => {
    let timedOut = false
    const timer = setTimeout(() => {
      timedOut = true
      const error = new BenchmarkTimeoutError(operationName, timeoutMs)
      error.lateOperation = lateOperation
      reject(error)
    }, timeoutMs)

    lateOperation.then((result) => {
      if (timedOut) return
      clearTimeout(timer)
      if (result.status === "fulfilled") resolve(result.value)
      else reject(result.error)
    })
  })
}

export function isTimeout(error) {
  return Boolean(error && error.code === "BENCHMARK_TIMEOUT")
}

/** Wait for a timed-out operation, but never wait beyond the supplied bound. */
export async function waitForLateOperation(error, timeoutMs) {
  if (!isTimeout(error) || !error.lateOperation) {
    return { status: "not-applicable" }
  }
  return waitForPromiseSettlement(error.lateOperation, timeoutMs)
}

/** Wait for a previously retained late-operation observer. */
export async function waitForPromiseSettlement(promise, timeoutMs) {
  if (!Number.isInteger(timeoutMs) || timeoutMs <= 0) {
    return { status: "pending" }
  }
  let timer
  const timeout = new Promise((resolve) => {
    timer = setTimeout(() => resolve({ status: "pending" }), timeoutMs)
  })
  const result = await Promise.race([promise, timeout])
  clearTimeout(timer)
  return result
}

function redact(value) {
  return String(value)
    .replace(/(https?:\/\/[^\s/@:]+:)[^\s/@]+@/gi, "$1[REDACTED]@")
    .replace(
      /([?&](?:access[_-]?key|secret(?:[_-]?access)?[_-]?key|token|password|credential|signature|authorization)=)[^&\s]*/gi,
      "$1[REDACTED]",
    )
    .replace(
      /\b(?:AWS_SECRET_ACCESS_KEY|AWS_ACCESS_KEY_ID|R2_SECRET_ACCESS_KEY|R2_ACCESS_KEY_ID|PGLITE_DATABASE_URL|DATABASE_URL)=\S+/g,
      (match) => `${match.slice(0, match.indexOf("=") + 1)}[REDACTED]`,
    )
}

/** Convert an arbitrary operation error to stable, non-secret JSON fields. */
export function errorRecord(error) {
  if (error && typeof error === "object") {
    const record = {
      name: typeof error.name === "string" ? error.name : "Error",
      message: redact(typeof error.message === "string" ? error.message : String(error)),
    }
    for (const key of ["code", "errno", "syscall", "path", "dest", "operation", "timeoutMs"]) {
      if (error[key] !== undefined && error[key] !== null) {
        record[key] = typeof error[key] === "string" ? redact(error[key]) : error[key]
      }
    }
    if (isTimeout(error)) record.lateOperation = "pending"
    return record
  }
  return { name: "Error", message: redact(error) }
}

export function usageError(message) {
  const error = new Error(message)
  error.code = "BENCHMARK_USAGE"
  return error
}
