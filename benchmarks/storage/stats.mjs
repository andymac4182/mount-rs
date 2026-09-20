/**
 * Statistics shared by the storage benchmark runner and its unit tests.
 *
 * The percentile and five-percent trimming rules intentionally match the
 * ComputeSDK storage reference at revision
 * 92fbbc9ba7739111899121195236acb4fc6a8bb5.
 */

export function round(value) {
  if (!Number.isFinite(value)) {
    throw new TypeError(`cannot round a non-finite value: ${value}`)
  }
  return Math.round(value * 100) / 100
}

export function percentile(sorted, percent) {
  if (!Array.isArray(sorted) || sorted.length === 0) return 0
  if (!Number.isFinite(percent) || percent < 0 || percent > 100) {
    throw new RangeError(`percentile must be between 0 and 100: ${percent}`)
  }
  const index = Math.ceil((percent / 100) * sorted.length) - 1
  return sorted[Math.min(Math.max(index, 0), sorted.length - 1)]
}

/**
 * ComputeSDK trims the top and bottom five percent when enough samples exist,
 * then computes median/p95/p99 over the remaining sorted values.
 */
export function computeStats(values) {
  if (!Array.isArray(values)) {
    throw new TypeError("stats input must be an array")
  }
  if (values.some((value) => !Number.isFinite(value) || value < 0)) {
    throw new TypeError("stats values must be finite, non-negative numbers")
  }
  if (values.length === 0) return { median: 0, p95: 0, p99: 0 }

  const sorted = [...values].sort((left, right) => left - right)
  const trimCount = Math.floor(sorted.length * 0.05)
  const trimmed =
    trimCount > 0 && sorted.length - 2 * trimCount > 0
      ? sorted.slice(trimCount, sorted.length - trimCount)
      : sorted

  const middle = Math.floor(trimmed.length / 2)
  const median =
    trimmed.length % 2 === 0
      ? (trimmed[middle - 1] + trimmed[middle]) / 2
      : trimmed[middle]

  return {
    median,
    p95: percentile(trimmed, 95),
    p99: percentile(trimmed, 99),
  }
}

export function roundStats(stats) {
  return {
    median: round(stats.median),
    p95: round(stats.p95),
    p99: round(stats.p99),
  }
}
