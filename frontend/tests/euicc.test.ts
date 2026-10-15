import assert from 'node:assert/strict'
import { test } from 'node:test'
import { api } from '../src/api/index.ts'
import type { LpacEvent, LpacPayload } from '../src/api/types.ts'
import { advanceDownload, downloadFailure, downloadedIccid, type DownloadProgress } from '../src/utils/euiccDownload.ts'

const event = (message: string, data: unknown = null, code = 0, type: LpacEvent['type'] = 'progress'): LpacEvent =>
  ({ type, payload: { message, data, code } })

void test('download stages follow LPAC and retain metadata and the final ICCID', () => {
  let progress: DownloadProgress = { step: 0 }
  const messages = [
    'es10b_get_euicc_challenge_and_info', 'es9p_initiate_authentication',
    'es10b_authenticate_server', 'es9p_authenticate_client', 'es10b_prepare_download',
    'es9p_get_bound_profile_package', 'es10b_load_bound_profile_package',
  ]
  const expected = [0, 1, 2, 2, 2, 3, 4]
  messages.forEach((message, index) => {
    progress = advanceDownload(progress, event(message))
    assert.equal(progress.step, expected[index])
  })
  progress = advanceDownload(progress, event('es8p_metadata_parse', { profileName: '测试 eSIM', iccid: 'metadata-iccid' }))
  assert.equal(progress.step, 4, 'Late metadata must not move progress backwards')
  assert.equal(progress.metadata?.profileName, '测试 eSIM')
  progress = advanceDownload(progress, event('success', { iccid: 'final-iccid', seqNumber: 29 }, 0, 'lpa'))
  assert.equal(downloadedIccid(progress), 'final-iccid')
})

void test('failed server connection stays at the connection stage despite cancellation output', () => {
  let progress = advanceDownload({ step: 0 }, event('es9p_initiate_authentication'))
  for (const message of ['es10b_cancel_session', 'es9p_cancel_session']) {
    progress = advanceDownload(progress, event(message))
    assert.equal(progress.step, 1)
  }
  progress = advanceDownload(progress, event('es9p_initiate_authentication', 'HTTP transport failed', -1, 'lpa'))
  assert.equal(progress.step, 1)
  assert.match(downloadFailure(progress.result, 'fallback'), /无法与服务器正常通信/)
})

void test('stream preserves UTF-8 and JSON boundaries and returns the final LPAC payload', async (t) => {
  const result: LpacPayload = { code: 0, message: 'success', data: { iccid: '8944476500020800251' } }
  const metadata = event('es8p_metadata_parse', { profileName: '测试 eSIM😀' })
  const raw = `${JSON.stringify(metadata)}\n`
  const bytes = new TextEncoder().encode([
    { type: 'output', text: raw }, { type: 'lpac', event: metadata },
    { type: 'lpac', event: { type: 'lpa', payload: result } },
    { type: 'complete', success: true, result },
  ].map((entry) => JSON.stringify(entry)).join('\n'))
  t.mock.method(globalThis, 'fetch', () => Promise.resolve(new Response(new ReadableStream<Uint8Array>({
    start(controller) {
      for (const byte of bytes) controller.enqueue(new Uint8Array([byte]))
      controller.close()
    },
  }))))
  let output = ''
  const events: LpacEvent[] = []
  assert.deepEqual(await api.downloadEuiccProfile({ activation_code: 'test' }, (text) => { output += text }, (entry) => { events.push(entry) }), result)
  assert.equal(output, raw)
  assert.deepEqual(events, [metadata, { type: 'lpa', payload: result }])
})

void test('an HTTP 200 or process completion without a successful final lpa result cannot report success', async (t) => {
  for (const completion of [
    { type: 'complete', success: true },
    { type: 'complete', success: true, result: { code: -1, message: 'failed', data: null } },
    { type: 'complete', success: false, message: 'HTTP transport failed', result: { code: -1, message: 'failed', data: null } },
    { type: 'complete', success: false, message: 'process failed', result: { code: 0, message: 'success', data: null } },
  ]) {
    t.mock.method(globalThis, 'fetch', () => Promise.resolve(new Response(JSON.stringify(completion))))
    await assert.rejects(api.downloadEuiccProfile({}, () => undefined))
    t.mock.restoreAll()
  }
  t.mock.method(globalThis, 'fetch', () => Promise.resolve(new Response('{"type":"output","text":"partial"}\n')))
  await assert.rejects(api.downloadEuiccProfile({}, () => undefined), /未收到最终结果/)
})

void test('nickname length counts Chinese and supplementary Unicode consistently and rejects invalid input before sending', async (t) => {
  let sent = ''
  t.mock.method(globalThis, 'fetch', (_url: string, options: RequestInit) => {
    assert.equal(typeof options.body, 'string')
    sent = options.body as string
    return Promise.resolve(Response.json({ status: 'ok', data: {} }))
  })
  const nickname = '测'.repeat(31) + '😀'
  await api.setEuiccNickname('123', nickname)
  assert.deepEqual(JSON.parse(sent), { iccid: '123', nickname })
  sent = ''
  await assert.rejects(api.setEuiccNickname('123', nickname + 'x'), /32/)
  await assert.rejects(api.setEuiccNickname('123', 'test\u0000'), /控制字符/)
  assert.equal(sent, '')
  await api.setEuiccNickname('123', '')
  assert.deepEqual(JSON.parse(sent), { iccid: '123', nickname: '' })
})
