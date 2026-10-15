import assert from 'node:assert/strict'
import { test } from 'node:test'
import { createSingleFlight } from '../src/utils/singleFlight.ts'
import { request } from '../src/api/request.ts'

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
