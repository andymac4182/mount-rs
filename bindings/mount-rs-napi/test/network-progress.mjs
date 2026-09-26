const stages = Object.freeze([
  "concurrent PUT fetch", "concurrent PUT response body",
  "concurrent GET fetch", "concurrent GET response body",
  "streamed PUT fetch", "streamed PUT response body",
  "streamed GET fetch", "streamed GET response body",
])
const fields = ["cpu_user_us", "cpu_system_us", "rss_current_bytes", "rss_lifetime_peak_bytes"]
const noop = Object.freeze({ begin() {}, settle() {}, finish() {}, receipt: () => null })
const integer = (value) => Number.isSafeInteger(value) && value >= 0
const defaultSample = () => {
  const usage = process.resourceUsage()
  return { cpu_user_us: usage.userCPUTime, cpu_system_us: usage.systemCPUTime,
    rss_current_bytes: process.memoryUsage.rss(), rss_lifetime_peak_bytes: usage.maxRSS * 1024 }
}

// Test-only local accounting. Promise fulfillment is not server acceptance.
export function createNetworkProgress({ provider, concurrency, enabled = true,
  sample = defaultSample,
  clock = () => ({ unix_ms: Date.now(), monotonic_ms: performance.now() }),
  write = (line) => process.stderr.write(line),
  schedule = (callback, interval) => setInterval(callback, interval),
  cancel = (handle) => clearInterval(handle),
} = {}) {
  if (!enabled) return noop
  if (!["node-fs", "sqlite"].includes(provider) || !integer(concurrency) || concurrency < 1 || concurrency > 64) {
    throw new RangeError("network progress requires a fixed provider and concurrency in 1..64")
  }
  const rows = stages.map((stage) => ({ stage, issued: 0, fulfilled: 0, rejected: 0 }))
  let reason = null, timer = null, terminal = false, status = "observing", attempts = 0, last = null
  let failureStage = null, failureIndex = null, failureCategory = null
  const failObserver = (value) => {
    reason ??= value
    if (last) {
      last = Object.freeze({ ...last, available: false, reason,
        cpu_user_us: null, cpu_system_us: null,
        rss_current_bytes: null, rss_lifetime_peak_bytes: null })
    }
  }
  const readClock = () => {
    try {
      const source = clock()
      const value = { unix_ms: source.unix_ms, monotonic_ms: source.monotonic_ms }
      if (!integer(value.unix_ms) || !Number.isFinite(value.monotonic_ms) || value.monotonic_ms < 0) throw new Error()
      return value
    } catch { failObserver("clock_unavailable"); return null }
  }
  const readSample = () => {
    try {
      const source = sample()
      const value = Object.fromEntries(fields.map((field) => [field, source[field]]))
      if (!fields.every((field) => integer(value[field]))) throw new Error()
      return value
    } catch { failObserver("resource_unavailable"); return null }
  }
  const baselineClock = readClock(), baselineSample = readSample()
  const stop = () => {
    if (timer === null) return
    const handle = timer
    timer = null
    try { cancel(handle) } catch { failObserver("timer_unavailable") }
  }
  const emit = () => {
    if (attempts >= 16) { failObserver("publication_cap"); stop() }
    const time = readClock(), current = readSample()
    if (time && baselineClock && time.monotonic_ms < baselineClock.monotonic_ms) failObserver("clock_unavailable")
    if (current && baselineSample && (current.cpu_user_us < baselineSample.cpu_user_us || current.cpu_system_us < baselineSample.cpu_system_us)) failObserver("resource_unavailable")
    const measured = !reason && baselineClock && baselineSample && time && current
    last = Object.freeze({
      schema: "mount-rs.network-test-progress.v1", pid: process.pid, platform: process.platform,
      provider, concurrency, sequence: Math.min(attempts + 1, 16),
      published_unix_ms: time?.unix_ms ?? null,
      elapsed_ms: time && baselineClock ? Math.max(0, Math.floor(time.monotonic_ms - baselineClock.monotonic_ms)) : null,
      cadence_ms: 5000, status, terminal, available: Boolean(measured), reason,
      cpu_user_us: measured ? current.cpu_user_us - baselineSample.cpu_user_us : null,
      cpu_system_us: measured ? current.cpu_system_us - baselineSample.cpu_system_us : null,
      rss_current_bytes: measured ? current.rss_current_bytes : null,
      rss_lifetime_peak_bytes: measured ? current.rss_lifetime_peak_bytes : null,
      cpu_scope: "node_process_delta_since_observer_start",
      rss_scope: "node_process_including_embedded_addon; peak_is_process_lifetime",
      requests_scope: "client_promise_issued_and_settled; not_server_acceptance_or_http_success",
      application_drain_proven: false, capture_atomic: false,
      failure_stage: failureStage, failure_index: failureIndex, failure_category: failureCategory,
      rows: Object.freeze(rows.map((row) => Object.freeze({ ...row, in_flight: row.issued - row.fulfilled - row.rejected }))),
    })
    if (attempts >= 16 || reason === "sink_failed" || reason === "sink_backpressure") return
    const line = `network_test_progress ${JSON.stringify(last)}\n`
    if (Buffer.byteLength(line) > 4096) { failObserver("output_cap"); stop(); return }
    attempts++
    try {
      if (write(line) === false) { failObserver("sink_backpressure"); stop() }
    } catch { failObserver("sink_failed"); stop() }
  }
  const finish = (outcome) => {
    if (terminal) return
    terminal = true
    status = outcome === "passed" ? "passed" : "failed"
    stop()
    emit()
  }
  emit()
  if (reason !== "sink_failed" && reason !== "sink_backpressure") {
    try {
      timer = schedule(() => { if (!terminal && timer !== null) emit() }, 5000)
      timer?.unref?.()
    } catch { failObserver("timer_unavailable"); stop() }
  }
  return Object.freeze({
    begin(stage) {
      if (terminal) return
      const row = rows[stages.indexOf(stage)]
      if (!row || row.issued >= 65) { failObserver("invalid_event"); return }
      row.issued++
    },
    settle(stage, outcome, index, error) {
      if (terminal) return
      const row = rows[stages.indexOf(stage)]
      if (!row || (outcome !== "fulfilled" && outcome !== "rejected") || row.fulfilled + row.rejected >= row.issued) { failObserver("invalid_event"); return }
      row[outcome]++
      if (outcome === "rejected") {
        failureStage = row.stage
        failureIndex = integer(index) && index < concurrency ? index : null
        let name
        try { name = error?.name } catch {}
        failureCategory = name === "TimeoutError" ? "timeout" : name === "AbortError" ? "aborted" : "request_error"
        finish("failed")
      }
    },
    finish,
    receipt: () => last,
  })
}
