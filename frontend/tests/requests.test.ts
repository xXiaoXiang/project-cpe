import assert from 'node:assert/strict'
import { test } from 'node:test'
import { createSingleFlight } from '../src/utils/singleFlight.ts'
import { request } from '../src/api/request.ts'
import { createModemReadPolicy } from '../src/api/modemPolling.ts'
import type { ApiResponse } from '../src/api/types.ts'

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (reason: Error) => void
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no })
  return { promise, resolve, reject }
}

void test('slow requests survive repeated refreshes while fast requests keep updating', async () => {
  const run = createSingleFlight()
  const slow = deferred<string>()
  let slowCalls = 0
  const updates: string[] = []
  const readSlow = () => run('slow', async () => {
    slowCalls++
    const result = await slow.promise
    updates.push(result)
  })
  const pending = readSlow()
  for (let tick = 0; tick < 10; tick++) {
    assert.equal(readSlow(), pending)
    await run('fast', () => Promise.resolve().then(() => { updates.push(`fast-${tick}`) }))
  }
  assert.equal(slowCalls, 1)
  assert.equal(updates.length, 10)
  slow.resolve('slow-result')
  await pending
  assert.equal(updates.at(-1), 'slow-result')
  await readSlow()
  assert.equal(slowCalls, 2)
})

void test('failed requests release their key for the next refresh', async () => {
  const run = createSingleFlight()
  const failure = deferred<string>()
  const pending = run('sim', () => failure.promise)
  const assertion = assert.rejects(pending, /offline/)
  failure.reject(new Error('offline'))
  await assertion
  assert.equal(await run('sim', () => Promise.resolve('recovered')), 'recovered')
})

void test('HTTP reads share pending responses, including body parsing; writes stay independent', async (t) => {
  const body = deferred<{ value: string }>()
  const calls: string[] = []
  t.mock.method(globalThis, 'fetch', (url: string, options: RequestInit) => {
    calls.push(`${options.method ?? 'GET'} ${url}`)
    return Promise.resolve({ ok: true, json: () => body.promise } as Response)
  })
  const first = request('/test-slow')
  await Promise.resolve()
  for (let tick = 0; tick < 10; tick++) assert.equal(request('/test-slow'), first)
  assert.equal(calls.length, 1)
  const writes = [request('/test-slow', { method: 'POST' }), request('/test-slow', { method: 'POST' })]
  assert.equal(calls.length, 3)
  body.resolve({ value: 'finished' })
  assert.deepEqual(await first, { value: 'finished' })
  await Promise.all(writes)
  await request('/test-slow')
  assert.equal(calls.length, 4)
})

void test('HTTP failures can recover on a later refresh without an overlapping automatic retry', async (t) => {
  let calls = 0
  t.mock.method(globalThis, 'fetch', () => {
    calls++
    return Promise.resolve(calls === 1
      ? new Response('', { status: 503 })
      : Response.json({ value: 'recovered' }))
  })
  await assert.rejects(request('/test-recovery'), /503/)
  assert.equal(calls, 1)
  assert.deepEqual(await request('/test-recovery'), { value: 'recovered' })
  assert.equal(calls, 2)
})

void test('modem diagnostics wait for stable registration, pause in airplane mode and resume', async () => {
  let clock = 1000
  let online = true
  let registration = 'searching'
  const calls: string[] = []
  const policy = createModemReadPolicy(() => clock)
  const read = (url: string): Promise<ApiResponse<unknown>> => {
    calls.push(url)
    return Promise.resolve({ status: 'ok', message: '', data: url === '/airplane-mode'
      ? { enabled: !online, online, powered: true }
      : url === '/network' ? { registration_status: registration } : { value: 'diagnostic' } })
  }
  const query = (url: string) => policy.read<ApiResponse<unknown>>(url, read)
  assert.equal((await query('/cells')).status, 'deferred')
  for (let tick = 0; tick < 4; tick++) { clock += 1000; await query('/qos') }
  assert.equal(calls.filter((url) => url === '/network').length, 1)
  assert.equal(calls.includes('/cells'), false)
  registration = 'registered'
  clock += 1000
  assert.equal((await query('/cells')).status, 'deferred')
  clock += 3000
  assert.equal((await query('/cells')).status, 'ok')
  online = false
  clock += 5000
  assert.equal((await query('/qos')).status, 'deferred')
  assert.equal(calls.includes('/qos'), false)
  online = true
  clock += 5000
  await query('/qos')
  clock += 3000
  assert.equal((await query('/qos')).status, 'ok')
})

void test('InProgress backs off all modem reads for 5/10/20/30 seconds without blocking stats', async () => {
  let clock = 1000
  let busy = true
  const calls: string[] = []
  const policy = createModemReadPolicy(() => clock)
  const read = (url: string): Promise<ApiResponse<unknown>> => {
    calls.push(url)
    if (url === '/airplane-mode') return Promise.resolve({ status: 'ok', message: '', data: { online: true, powered: true } })
    if (url === '/network') return Promise.resolve({ status: 'ok', message: '', data: { registration_status: 'registered' } })
    if (busy && url === '/sim') return Promise.resolve({ status: 'error', message: 'org.ofono.Error.InProgress: Operation already in progress' })
    return Promise.resolve({ status: 'ok', message: '', data: {} })
  }
  const query = (url: string) => policy.read<ApiResponse<unknown>>(url, read)
  for (const backoff of [5000, 10_000, 20_000, 30_000]) {
    const response = await query('/sim')
    assert.equal(response.status, 'deferred')
    assert.equal(response.retry_after_ms, backoff)
    const before = calls.length
    clock += backoff - 1
    await query('/sim')
    await query('/qos')
    assert.equal(calls.length, before)
    assert.equal((await query('/stats')).status, 'ok')
    clock++
    assert.equal((await query('/device/imeisv')).status, 'ok', 'An unrelated success must not reset busy QoS/SIM backoff')
  }
  busy = false
  assert.equal((await query('/sim')).status, 'ok')
})

void test('SIM changes discard old responses and keep writes out of the background queue', async () => {
  let clock = 1000
  const old = deferred<ApiResponse<unknown>>()
  const policy = createModemReadPolicy(() => clock)
  const read = async (url: string): Promise<ApiResponse<unknown>> => url === '/airplane-mode'
    ? { status: 'ok', message: '', data: { online: true, powered: true } }
    : url === '/network' ? { status: 'ok', message: '', data: { registration_status: 'registered' } }
      : old.promise
  const pending = policy.read<ApiResponse<unknown>>('/sim', read)
  // Let the readiness probe complete and the SIM request enter its pending state.
  await new Promise<void>((resolve) => setImmediate(resolve))
  policy.beginTransition()
  assert.equal((await policy.read<ApiResponse<unknown>>('/sim', read)).status, 'deferred')
  old.resolve({ status: 'ok', message: '', data: { iccid: 'OLD' } })
  assert.equal((await pending).status, 'deferred')
  policy.endTransition()
  assert.equal((await policy.read<ApiResponse<unknown>>('/sim', read)).status, 'deferred')
  clock += 3000
  assert.equal((await policy.read<ApiResponse<unknown>>('/sim', read)).status, 'ok')
})

void test('interface recovery is deferred, while genuine modem errors remain visible', async () => {
  let clock = 1000
  const policy = createModemReadPolicy(() => clock)
  const read = (url: string): Promise<ApiResponse<unknown>> => Promise.resolve(url === '/airplane-mode'
    ? { status: 'ok', message: '', data: { online: true, powered: true } }
    : { status: 'deferred', message: 'NetworkRegistration is restarting', retry_after_ms: 5000 })
  assert.equal((await policy.read<ApiResponse<unknown>>('/cells', read)).status, 'deferred')
  clock += 5000
  await assert.rejects(policy.read('/cells', () => Promise.resolve({ status: 'error', message: 'DBus connection lost' })), /DBus connection lost/)
})
