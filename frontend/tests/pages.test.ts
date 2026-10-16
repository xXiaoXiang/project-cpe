import assert from 'node:assert/strict'
import { once } from 'node:events'
import { existsSync } from 'node:fs'
import { mkdtemp, readFile } from 'node:fs/promises'
import { createServer, type ServerResponse } from 'node:http'
import { tmpdir } from 'node:os'
import { dirname, extname, join, resolve, sep } from 'node:path'
import { spawn } from 'node:child_process'
import process from 'node:process'
import { setTimeout as delay } from 'node:timers/promises'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'
import type { EuiccNotification, EuiccProfile, LpacEvent } from '../src/api/types.ts'

// Run after building. Uses a local mock API and Chromium's DevTools protocol;
// no device, browser automation package, or real mutation API is involved.
const browserPath = [process.env.BROWSER_PATH,
  'C:/Program Files/Google/Chrome/Application/chrome.exe',
  'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
].find((candidate) => candidate && existsSync(candidate))
const dist = resolve(dirname(fileURLToPath(import.meta.url)), '../dist')

const fixtures: Record<string, unknown> = {
  '/refresh/config': { interval_ms: 1000 },
  '/refresh/heartbeat': { interval_ms: 1000 },
  '/device': { imei: '123456789012345', manufacturer: 'FAST_DEVICE', model: 'MODEL', powered: true, online: true },
  '/sim': { present: true, iccid: 'SLOW_SIM', imsi: '123', phone_numbers: [], preferred_languages: [] },
  '/sim/slot': { active_slot: 1, raw_value: '66051' },
  '/device/imeisv': { software_version_number: '01' },
  '/radio-mode': { mode: 'auto' },
  '/band-lock': { locked: false, lte_fdd_bands: [], lte_tdd_bands: [], nr_fdd_bands: [], nr_tdd_bands: [] },
  '/network/operators': { operators: [{ name: 'FAST_OPERATOR', status: 'current', technologies: ['LTE'] }] },
  '/cells': { serving_cell: { tech: 'SLOW_NETWORK', cell_id: 123, tac: 456 }, cells: [] },
  '/usb-mode': { current_mode: 1, current_mode_name: 'FAST_USB' },
  '/data': { active: true },
  '/airplane-mode': { enabled: false, online: true, powered: true },
  '/network': { operator_name: 'TEST', registration_status: 'registered', technology_preference: 'NR 5G/LTE auto', signal_strength: 75, mcc: '454', mnc: '03' },
  '/stats': {},
  '/qos': { qci: 9, dl_speed: 30000, ul_speed: 10000 },
  '/ims/status': { registered: true },
  '/roaming': { is_roaming: false, roaming_allowed: true },
  '/connectivity': { ipv4: { success: true, latency_ms: 20 }, ipv6: { success: false } },
  '/webhook/config': { enabled: true, url: 'https://slow.example/', headers: {} },
  '/sms/list': [{ id: 1, direction: 'incoming', phone_number: '123', content: 'FAST_SMS', timestamp: '2026-10-14T00:00:00Z', status: 'received' }],
  '/sms/stats': { total: 987, incoming: 987, outgoing: 0 },
  '/calls': { calls: [{ path: '/call/1', phone_number: 'FAST_CALL', state: 'active', direction: 'incoming' }] },
  '/call/history': { records: [{ id: 1, phone_number: 'SLOW_HISTORY', direction: 'incoming', duration: 1, start_time: '2026-10-14T00:00:00Z', answered: true }], stats: { total: 1, incoming: 1, outgoing: 0, missed: 0, total_duration: 1 } },
  '/euicc': { eid: '12345678901234567890123456789012', firmware_version: null, profiles: [
    { iccid: '8944476500020800251', isdpAid: 'test', profileState: 'disabled', profileNickname: '旧昵称',
      serviceProviderName: 'BetterRoaming', profileName: 'BetterRoaming', iconType: null, icon: null, profileClass: 'operational' },
    { iccid: '8985203130001133747', isdpAid: 'active', profileState: 'enabled', profileNickname: null,
      serviceProviderName: '3', profileName: '3', iconType: null, icon: null, profileClass: 'operational' },
    { iccid: '8965012605253617847', isdpAid: 'trip', profileState: 'disabled', profileNickname: '旅行卡',
      serviceProviderName: 'Trip', profileName: 'Singtel', iconType: null, icon: null, profileClass: 'provisioning' },
  ] },
}

void test('pages update fast endpoints while slow responses survive 1-second refreshes', {
  skip: !browserPath && 'Set BROWSER_PATH to an installed Chromium browser',
  timeout: 90_000,
}, async (t) => {
  assert.ok(existsSync(join(dist, 'index.html')), 'Build frontend before running page tests')
  let slowPath = ''
  let counts = new Map<string, number>()
  const held = new Set<ServerResponse>()
  let aborted = 0
  let downloadResponse: ServerResponse | undefined
  let qosError = ''
  let selectionWarning: string | null = null
  const controlWrites: { path: string; payload: Record<string, boolean>; response: ServerResponse }[] = []
  let notifications: EuiccNotification[] = [
    { seqNumber: 29, iccid: '8965012512294623745', profileManagementOperation: 'future-operation' },
    { seqNumber: 30, iccid: '8965012512294623745', profileManagementOperation: 'install' },
    { seqNumber: 27, iccid: '8985203130001133747', profileManagementOperation: 'enable' },
    { seqNumber: 28, iccid: '8965012512294623745', profileManagementOperation: 'delete' },
    { seqNumber: 26, iccid: '8965012512294623745', profileManagementOperation: 'disable' },
  ]
  const actions: { path: string; body: Record<string, unknown> }[] = []
  const jsonReply = (response: ServerResponse, data: unknown) => {
    response.writeHead(200, { 'Content-Type': 'application/json' })
    response.end(JSON.stringify({ status: 'ok', data }))
  }
  const reply = (response: ServerResponse, path: string) => {
    const data = fixtures[path]
    response.writeHead(200, { 'Content-Type': 'application/json' })
    response.end(JSON.stringify(data === undefined
      ? { status: 'error', message: `No fixture: ${path}` }
      : { status: 'ok', message: 'Success', data }))
  }
  const server = createServer((req, res) => {
    const path = new URL(req.url ?? '/', 'http://localhost').pathname
    if (path.startsWith('/api/')) {
      const endpoint = path.slice(4)
      counts.set(endpoint, (counts.get(endpoint) ?? 0) + 1)
      if (req.method === 'POST' && ['/data', '/roaming', '/airplane-mode'].includes(endpoint)) {
        void (async () => {
          let body = ''
          for await (const chunk of req) body += String(chunk)
          controlWrites.push({ path: endpoint, payload: JSON.parse(body) as Record<string, boolean>, response: res })
        })().catch(() => { res.writeHead(500); res.end() })
        return
      }
      if (endpoint === '/qos' && qosError) {
        res.writeHead(200, { 'Content-Type': 'application/json' })
        res.end(JSON.stringify({ status: 'error', message: qosError }))
        return
      }
      if (endpoint === '/euicc/notifications') { jsonReply(res, notifications); return }
      if (endpoint.startsWith('/euicc/') && req.method === 'POST') {
        void (async () => {
          let body = ''
          for await (const chunk of req) body += String(chunk)
          const payload = JSON.parse(body) as Record<string, unknown>
          actions.push({ path: endpoint, body: payload })
          if (endpoint === '/euicc/profile/download') {
            res.writeHead(200, { 'Content-Type': 'application/x-ndjson' })
            res.flushHeaders()
            downloadResponse = res
          } else if (endpoint === '/euicc/notifications/remove') {
            const removed = notifications.filter((item) => payload.all
              ? ['enable', 'disable'].includes(item.profileManagementOperation) : item.seqNumber === payload.seq_number)
            notifications = notifications.filter((item) => !removed.includes(item))
            jsonReply(res, { removed: removed.map((item) => item.seqNumber), failed: [], skipped: payload.all ? notifications.length : 0 })
          } else if (endpoint === '/euicc/profile/nickname') {
            jsonReply(res, { iccid: payload.iccid, nickname: payload.nickname })
          } else if (endpoint === '/euicc/profile/enable') {
            const info = fixtures['/euicc'] as { profiles: EuiccProfile[] }
            info.profiles = info.profiles.map((profile) => ({ ...profile,
              profileState: profile.iccid === payload.iccid ? 'enabled' : 'disabled' }))
            jsonReply(res, { iccid: payload.iccid, profiles: info.profiles, network_selection_warning: selectionWarning })
          } else jsonReply(res, {})
        })().catch(() => { res.writeHead(500); res.end() })
        return
      }
      if (endpoint === slowPath) {
        held.add(res)
        res.on('close', () => {
          if (!res.writableEnded) aborted++
          held.delete(res)
        })
      } else {
        reply(res, endpoint)
      }
      return
    }
    const asset = resolve(dist, `.${path}`)
    const file = asset.startsWith(`${dist}${sep}`) && extname(asset) ? asset : join(dist, 'index.html')
    void readFile(file).then((data) => {
      const mime: Record<string, string> = { '.html': 'text/html', '.js': 'application/javascript', '.css': 'text/css', '.svg': 'image/svg+xml' }
      res.writeHead(200, { 'Content-Type': mime[extname(file)] ?? 'application/octet-stream' })
      res.end(data)
    }).catch(() => { res.writeHead(404); res.end() })
  })
  server.listen(0, '127.0.0.1')
  await once(server, 'listening')
  const address = server.address()
  assert.ok(address && typeof address !== 'string')
  const origin = `http://127.0.0.1:${address.port}`
  const profile = await mkdtemp(join(tmpdir(), 'cpe-polling-test-'))
  const browser = spawn(browserPath as string, [
    '--headless=new', '--disable-gpu', '--no-first-run', '--no-default-browser-check',
    '--remote-debugging-port=0', `--user-data-dir=${profile}`, 'about:blank',
  ], { windowsHide: true, stdio: ['ignore', 'ignore', 'pipe'] })
  t.after(() => {
    browser.kill()
    server.closeAllConnections()
    server.close()
  })
  const endpoint = await new Promise<string>((yes, no) => {
    let stderr = ''
    const timeout = setTimeout(() => no(new Error(`Browser did not start: ${stderr}`)), 15_000)
    browser.once('error', no)
    browser.stderr.on('data', (chunk: Buffer) => {
      stderr += chunk.toString()
      const match = /DevTools listening on (ws:\/\/[^\s]+)/.exec(stderr)
      if (match) { clearTimeout(timeout); yes(match[1]) }
    })
  })
  const devtoolsOrigin = new URL(endpoint).origin.replace('ws:', 'http:')
  const targets = await (await fetch(`${devtoolsOrigin}/json/list`)).json() as { webSocketDebuggerUrl: string; type: string }[]
  const page = targets.find((target) => target.type === 'page')
  assert.ok(page)
  const socket = new WebSocket(page.webSocketDebuggerUrl)
  await new Promise<void>((yes, no) => { socket.addEventListener('open', () => yes()); socket.addEventListener('error', no) })
  t.after(() => socket.close())
  let id = 0
  const pending = new Map<number, { yes: (value: Record<string, unknown>) => void; no: (error: Error) => void }>()
  socket.addEventListener('message', (event) => {
    const message = JSON.parse(String(event.data)) as { id: number; result: Record<string, unknown>; error?: { message: string } }
    const operation = pending.get(message.id)
    if (!operation) return
    pending.delete(message.id)
    if (message.error) operation.no(new Error(message.error.message))
    else operation.yes(message.result)
  })
  const call = (method: string, params: Record<string, unknown> = {}) => new Promise<Record<string, unknown>>((yes, no) => {
    const nextId = ++id
    const timeout = setTimeout(() => {
      pending.delete(nextId)
      no(new Error(`DevTools timed out: ${method}`))
    }, 5000)
    pending.set(nextId, {
      yes: (value) => { clearTimeout(timeout); yes(value) },
      no: (error) => { clearTimeout(timeout); no(error) },
    })
    socket.send(JSON.stringify({ id: nextId, method, params }))
  })
  const evaluate = async (expression: string) => {
    const result = await call('Runtime.evaluate', { expression, returnByValue: true })
    assert.equal(result.exceptionDetails, undefined, `Page evaluation failed: ${expression}`)
    return (result.result as { value: unknown }).value
  }
  const waitFor = async (expression: string) => {
    const until = Date.now() + 10_000
    while (Date.now() < until) {
      if (await evaluate(expression)) return
      await delay(100)
    }
    assert.fail(`Page never satisfied: ${expression}; ${String(await evaluate('document.body.innerText'))}`)
  }
  const textHas = (value: string) => `document.body.innerText.includes(${JSON.stringify(value)})`
  const clickTab = (label: string) => evaluate(`Array.from(document.querySelectorAll('[role="tab"]')).find(x => x.innerText.includes(${JSON.stringify(label)}))?.click()`)

  const cases = [
    { route: '/', slow: '/sim', fast: '/device', marker: 'FAST_DEVICE', slowMarker: 'SLOW_SIM' },
    { route: '/device', slow: '/sim', fast: '/device', marker: 'FAST_DEVICE', slowMarker: 'SLOW_SIM' },
    { route: '/network', slow: '/cells', fast: '/network/operators', marker: 'FAST_OPERATOR', slowMarker: 'SLOW_NETWORK', tab: '运营商管理', slowTab: '小区与锁定' },
    { route: '/config', slow: '/webhook/config', fast: '/usb-mode', marker: 'FAST_USB', slowMarker: '', manual: true },
    { route: '/sms', slow: '/sms/stats', fast: '/sms/list', marker: 'FAST_SMS', slowMarker: '987' },
    { route: '/phone', slow: '/call/history', fast: '/calls', marker: 'FAST_CALL', slowMarker: 'SLOW_HISTORY', slowTab: '通话记录' },
  ]
  for (const scenario of cases) {
    await t.test(scenario.route, async () => {
      slowPath = scenario.slow
      counts = new Map()
      aborted = 0
      await call('Page.navigate', { url: `${origin}${scenario.route}` })
      await waitFor('document.querySelector("main") !== null')
      if (scenario.tab) {
        await waitFor('document.querySelector("[role=tab]") !== null')
        await clickTab(scenario.tab)
      }
      await waitFor(textHas(scenario.marker))
      const startedBy = Date.now() + 6000
      while (!counts.get(scenario.slow) && Date.now() < startedBy) await delay(100)
      assert.equal(counts.get(scenario.slow), 1)
      if (scenario.manual) {
        // Config forms refresh manually, so click the global refresh button twice.
        await evaluate(`document.querySelector('button[title="刷新页面"]').click()`)
        await delay(150)
        await evaluate(`document.querySelector('button[title="刷新页面"]').click()`)
      }
      await delay(2500)
      assert.equal(counts.get(scenario.slow), 1, 'Pending request must not restart each tick')
      assert.equal(aborted, 0, 'Pending request must not be canceled')
      assert.ok((counts.get(scenario.fast) ?? 0) >= 2, 'Fast endpoints must continue refreshing')
      if (scenario.slowTab) await clickTab(scenario.slowTab)
      if (scenario.route === '/config') {
        await evaluate(`Array.from(document.querySelectorAll('.MuiAccordionSummary-root')).find(x => x.innerText.includes('Webhook 转发'))?.click()`)
      }
      for (const response of held) reply(response, scenario.slow)
      held.clear()
      await waitFor(scenario.route === '/config'
        ? `Array.from(document.querySelectorAll('input')).some(x => x.value === 'https://slow.example/')`
        : textHas(scenario.slowMarker))
    })
  }

  await t.test('carrier logos and HK badges follow the current registered PLMN', async () => {
    const originalNetwork = fixtures['/network']
    slowPath = ''
    await call('Page.navigate', { url: `${origin}/` })
    const overview = 'document.querySelector("[data-network-overview]")'
    const logo = `${overview}?.querySelector('img')`
    for (const [mcc, mnc, name, file, badge] of [
      ['454', '03', '3HK', 'three_hk.svg', false],
      ['454', '12', '中国移动香港', 'china-mobile.svg', true],
      ['460', '00', '中国移动', 'china-mobile.svg', false],
      ['454', '16', 'HKT', 'hkt.svg', false],
    ] as const) {
      fixtures['/network'] = { ...(originalNetwork as object), mcc, mnc }
      await waitFor(`${logo}?.alt === ${JSON.stringify(name)}`)
      assert.equal(await evaluate(`${logo}.getAttribute('src')`), `/provider/${file}`)
      await waitFor(`${logo}.complete && ${logo}.naturalWidth > 0`)
      assert.equal(await evaluate(`${overview}.innerText.includes('HK')`), badge)
    }
    fixtures['/network'] = originalNetwork
  })

  await t.test('modem startup, busy recovery, airplane display and actual technology', async () => {
    slowPath = '/cells'
    counts = new Map()
    await call('Page.navigate', { url: `${origin}/` })
    const overview = 'document.querySelector("[data-network-overview]")'
    await waitFor(`${overview}?.innerText.includes('已注册')`)
    assert.equal(await evaluate(`${overview}.innerText.includes('5G')`), false, 'Allowed NR mode is not the current network type')
    assert.equal(await evaluate(`Array.from(${overview}.querySelectorAll('.MuiChip-label')).some(x => x.innerText === 'LTE')`), false)
    await waitFor(textHas('SLOW_SIM'))
    const simField = 'Array.from(document.querySelectorAll("main .MuiTypography-root")).find(x => x.innerText === "SLOW_SIM")'
    await waitFor(`${simField} !== undefined`)
    assert.equal(await evaluate(`getComputedStyle(${simField}).filter`), 'none')
    await evaluate(`Array.from(document.querySelectorAll('.MuiCard-root')).find(x => x.innerText.includes('SIM 卡信息')).querySelector('button').click()`)
    await waitFor(`getComputedStyle(${simField}).filter === 'blur(5px)'`)
    await evaluate(`document.querySelector('button[title="隐藏所有敏感信息"]').click()`)
    await evaluate(`document.querySelector('button[title="显示所有敏感信息"]').click()`)
    await waitFor(`getComputedStyle(${simField}).filter === 'none'`)
    await waitFor('document.querySelector("main") !== null')
    const startedBy = Date.now() + 6000
    while (!held.size && Date.now() < startedBy) await delay(100)
    const oldCells = fixtures['/cells']
    fixtures['/cells'] = { serving_cell: { tech: 'lte', cell_id: 123, tac: 456 }, cells: [] }
    for (const response of held) reply(response, '/cells')
    held.clear()
    slowPath = ''
    await waitFor(`${overview}?.innerText.includes('LTE')`)
    fixtures['/airplane-mode'] = { enabled: true, powered: true, online: false }
    // Deliberately leave network/device fixtures registered/online to expose stale data conflicts.
    await waitFor(`${overview}?.innerText.includes('飞行模式')`)
    for (const stale of ['已注册', '已上线', '已离线', 'LTE', '75%', 'VoLTE']) {
      assert.equal(await evaluate(`${overview}.innerText.includes(${JSON.stringify(stale)})`), false)
    }
    assert.equal(await evaluate(`${overview}.querySelector('img') !== null`), false)
    assert.equal(await evaluate(`${overview}.querySelector('[data-searching="true"]') !== null`), false)

    fixtures['/airplane-mode'] = { enabled: false, powered: true, online: true }
    const originalNetwork = fixtures['/network']
    fixtures['/network'] = { ...(originalNetwork as object), registration_status: 'searching' }
    counts = new Map()
    await call('Page.navigate', { url: `${origin}/` })
    const technology = `${overview}?.querySelector('[data-network-technology]')`
    await waitFor(`${technology}?.dataset.searching === 'true'`)
    assert.equal(await evaluate(`${technology}.innerText`), '-')
    assert.equal(await evaluate(`${overview}.innerText.includes('正在搜网')`), false)
    assert.equal(await evaluate('document.querySelector(".MuiAlert-standardInfo") !== null'), false)
    const border = `${technology}.querySelector('rect')`
    assert.equal(await evaluate(`getComputedStyle(${border}).animationDuration`), '1.8s')
    assert.equal(await evaluate(`getComputedStyle(${border}).animationIterationCount`), 'infinite')
    await call('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: 'reduce' }] })
    assert.equal(await evaluate(`getComputedStyle(${border}).animationName`), 'none')
    await call('Emulation.setEmulatedMedia', { features: [] })
    fixtures['/network'] = { ...(originalNetwork as object), registration_status: 'searching', technology: 'lte' }
    await waitFor(`${technology}?.innerText === 'LTE'`)
    assert.equal(await evaluate(`${technology}.dataset.searching`), 'true', 'Known technology remains visible while searching')
    await delay(2200)
    assert.equal(counts.get('/cells') ?? 0, 0)
    assert.equal(counts.get('/qos') ?? 0, 0)
    const statsBy = Date.now() + 5000
    while ((counts.get('/stats') ?? 0) < 2 && Date.now() < statsBy) await delay(100)
    assert.ok((counts.get('/stats') ?? 0) >= 2, `Stats did not refresh: ${JSON.stringify([...counts])}; visibility=${String(await evaluate('document.visibilityState'))}`)
    assert.equal(await evaluate(textHas('请求失败')), false)

    fixtures['/network'] = originalNetwork
    qosError = 'Failed to get QoS info: org.ofono.Error.InProgress: Operation already in progress'
    await waitFor(`${technology}?.dataset.searching === 'false'`)
    assert.equal(await evaluate(`${technology}.querySelector('svg rect') !== null`), false, 'Stop the border animation once registered')
    const attemptedBy = Date.now() + 10_000
    while (!counts.get('/qos') && Date.now() < attemptedBy) await delay(100)
    assert.equal(counts.get('/qos'), 1)
    const statsBefore = counts.get('/stats') ?? 0
    await delay(2200)
    assert.equal(counts.get('/qos'), 1, 'Busy QoS must not retry every second')
    assert.ok((counts.get('/stats') ?? 0) > statsBefore)
    assert.equal(await evaluate(textHas('请求失败')), false)
    qosError = ''
    const recoveredBy = Date.now() + 10_000
    while ((counts.get('/qos') ?? 0) < 2 && Date.now() < recoveredBy) await delay(100)
    assert.ok((counts.get('/qos') ?? 0) >= 2, 'QoS resumes automatically')
    qosError = 'QoS permission denied'
    await waitFor(textHas('请求失败'))
    qosError = ''
    fixtures['/cells'] = oldCells
  })

  await t.test('quick controls show pending, confirm POST results, reject stale polls and yield modem reads', async () => {
    slowPath = ''
    counts = new Map()
    fixtures['/data'] = { active: true }
    fixtures['/roaming'] = { roaming_allowed: false, is_roaming: false }
    await call('Page.navigate', { url: `${origin}/` })
    const input = (control: string) => `document.querySelector('[data-quick-control="${control}"] input')`
    const row = (control: string) => `document.querySelector('[data-quick-control="${control}"]')`
    await waitFor(`${input('roaming')} && !${input('roaming')}.disabled`)
    const begin = async (control: string, message: string, checked: boolean) => {
      const before = controlWrites.length
      await evaluate(`${input(control)}.click()`)
      await waitFor(`${row(control)}.getAttribute('aria-busy') === 'true' && ${row(control)}.innerText.includes(${JSON.stringify(message)})`)
      assert.equal(await evaluate(`${input(control)}.checked`), checked, 'Keep the confirmed switch position while pending')
      assert.equal(await evaluate('Array.from(document.querySelectorAll("[data-quick-control] input")).every(x => x.disabled)'), true)
      await evaluate(`${input(control)}.click()`)
      const deadline = Date.now() + 3000
      while (controlWrites.length === before && Date.now() < deadline) await delay(50)
      assert.equal(controlWrites.length, before + 1, 'Repeated clicks cannot issue another write')
      const action = controlWrites.at(-1)
      assert.ok(action)
      return action
    }
    const complete = async (control: string, data: unknown, checked: boolean) => {
      const action = controlWrites.at(-1)
      assert.ok(action)
      fixtures[action.path] = data
      jsonReply(action.response, data)
      await waitFor(`!${input(control)}.disabled && ${input(control)}.checked === ${checked}`)
      assert.equal(await evaluate(`${row(control)}.getAttribute('aria-busy')`), 'false')
    }

    const failure = await begin('data', '正在关闭', true)
    failure.response.writeHead(200, { 'Content-Type': 'application/json' })
    failure.response.end(JSON.stringify({ status: 'error', message: 'Data connection rejected' }))
    await waitFor(`!${input('data')}.disabled && ${input('data')}.checked`)
    await waitFor(textHas('请求失败'))
    await delay(1200)
    assert.equal(await evaluate(textHas('请求失败')), true, 'Stats polling must not clear a control failure')
    await begin('data', '正在关闭', true)
    await complete('data', { active: false }, false)

    // Hold a read from before the write, then release its old value after success.
    const oldRoaming = fixtures['/roaming']
    slowPath = '/roaming'
    const heldBy = Date.now() + 10_000
    while (!held.size && Date.now() < heldBy) await delay(100)
    assert.ok(held.size > 0)
    const roaming = await begin('roaming', '正在开启', false)
    assert.deepEqual(roaming.payload, { allowed: true })
    await delay(1200)
    await complete('roaming', { roaming_allowed: true, is_roaming: false }, true)
    slowPath = ''
    for (const response of held) jsonReply(response, oldRoaming)
    held.clear()
    await delay(250)
    assert.equal(await evaluate(`${input('roaming')}.checked`), true, 'Late old GET must not overwrite POST confirmation')

    for (const enabled of [true, false]) {
      const action = await begin('airplane', enabled ? '正在开启' : '正在关闭', !enabled)
      assert.deepEqual(action.payload, { enabled })
      const diagnosticCounts = ['/qos', '/cells', '/sim'].map((path) => counts.get(path) ?? 0)
      const statsBefore = counts.get('/stats') ?? 0
      await delay(2200)
      assert.deepEqual(['/qos', '/cells', '/sim'].map((path) => counts.get(path) ?? 0), diagnosticCounts)
      assert.ok((counts.get('/stats') ?? 0) > statsBefore, 'System stats continue during long radio writes')
      await complete('airplane', { enabled, online: !enabled, powered: true }, enabled)
    }
    const transportFailure = await begin('data', '正在开启', false)
    transportFailure.response.writeHead(503)
    transportFailure.response.end()
    await waitFor(`!${input('data')}.disabled && !${input('data')}.checked`)
    await waitFor(textHas('请求失败'))
  })

  await t.test('eUICC nickname, notification safety and live download progress', async () => {
    slowPath = ''
    await call('Emulation.setDeviceMetricsOverride', { width: 1440, height: 900, deviceScaleFactor: 1, mobile: false })
    await call('Page.navigate', { url: `${origin}/euicc` })
    await waitFor(textHas('BetterRoaming'))
    const click = (label: string) => evaluate(`Array.from(document.querySelectorAll('button')).find(x => x.getAttribute('aria-label') === ${JSON.stringify(label)} || x.innerText.trim() === ${JSON.stringify(label)})?.click()`)
    const fill = (label: string, value: string) => evaluate(`(() => {
      const input = Array.from(document.querySelectorAll('input')).find(x => document.querySelector('label[for="' + x.id + '"]')?.innerText.includes(${JSON.stringify(label)}));
      if (!input) throw new Error('Input not found');
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(input, ${JSON.stringify(value)});
      input.dispatchEvent(new Event('input', { bubbles: true }));
    })()`)

    assert.equal(await evaluate('document.querySelector(\'a[href="https://github.com/xXiaoXiang/project-cpe"]\') !== null'), true)
    assert.equal(await evaluate('document.querySelector(\'button[aria-label="删除 BetterRoaming"]\').innerText'), '删除')
    const tripCard = `document.querySelector('[data-profile="8965012605253617847"]')`
    const tripHeading = `${tripCard}.querySelector('[data-profile-heading]')`
    assert.deepEqual(await evaluate(`Array.from(${tripHeading}.querySelectorAll('.MuiTypography-root')).map(x => x.innerText)`), ['Singtel', 'Trip', '旅行卡'])
    assert.equal(await evaluate(`getComputedStyle(${tripHeading}.querySelector('h2')).fontWeight`), '700')
    assert.equal(await evaluate(`getComputedStyle(${tripHeading}.querySelector('[title="Trip"]')).fontWeight`), '400')
    assert.equal(await evaluate(`Array.from(document.querySelectorAll('[data-profile-heading]')).find(x => x.innerText.includes('BetterRoaming')).querySelectorAll('[title="BetterRoaming"]').length`), 1, 'Do not duplicate matching provider and profile names')
    assert.equal(await evaluate(`${tripCard}.innerText.includes('引导配置（provisioning）')`), true)
    assert.equal(await evaluate(`document.querySelector('[data-profile="8985203130001133747"]').innerText.includes('业务配置（operational）')`), true)
    assert.equal(await evaluate(`(() => {
      const cards = Array.from(document.querySelectorAll('[data-profile]'));
      const bounds = cards.map(x => x.getBoundingClientRect());
      return bounds.every(x => bounds.filter(y => Math.abs(x.top - y.top) < 1).every(y => Math.abs(x.height - y.height) < 1));
    })()`), true, 'Profile cards on the same row have equal heights')
    await click('启用 BetterRoaming')
    await waitFor(textHas('启用BetterRoaming会关闭3，确认是否继续？'))
    assert.equal(actions.filter((item) => item.path.endsWith('/enable')).length, 0)
    await click('取消')
    await click('编辑 BetterRoaming 的昵称')
    await waitFor('document.querySelector("input") !== null')
    await fill('昵称', '测'.repeat(33))
    await waitFor('document.querySelector("button[aria-label=保存昵称]").disabled')
    assert.equal(actions.filter((item) => item.path.endsWith('/nickname')).length, 0)
    await fill('昵称', '测试eSIM😀')
    await click('保存昵称')
    await waitFor(textHas('测试eSIM😀'))
    assert.equal(actions.at(-1)?.body.nickname, '测试eSIM😀')
    assert.equal(await evaluate('document.querySelector("h2").innerText'), 'BetterRoaming')
    await call('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: false })
    await waitFor(`Array.from(document.querySelectorAll('[data-profile]')).every(x => x.scrollWidth <= x.clientWidth)`)
    assert.equal(await evaluate(`(() => {
      const centers = Array.from(${tripHeading}.querySelectorAll('.MuiTypography-root')).map(x => {
        const rect = x.getBoundingClientRect();
        return rect.top + rect.height / 2;
      });
      return Math.max(...centers) - Math.min(...centers) < 1;
    })()`), true, 'Keep profile, provider and nickname on one line on narrow screens')
    await call('Emulation.clearDeviceMetricsOverride')

    selectionWarning = 'TEST: Modem selection mode is still manual'
    await click('启用 BetterRoaming')
    await waitFor(textHas('启用BetterRoaming会关闭3，确认是否继续？'))
    await evaluate(`document.querySelector('[role="dialog"] .MuiDialogActions-root button:last-child').click()`)
    await waitFor(textHas('Profile 已切换，但自动选网尚未确认'))
    assert.equal(await evaluate('document.querySelector(\'button[aria-label="启用 BetterRoaming"]\').disabled'), true, 'Keep the confirmed new profile enabled even if selection recovery fails')
    assert.equal(await evaluate(textHas('Profile 切换失败')), false)
    selectionWarning = null
    await click('启用 Singtel')
    await waitFor(textHas('启用Singtel会关闭BetterRoaming，确认是否继续？'))
    await evaluate(`document.querySelector('[role="dialog"] .MuiDialogActions-root button:last-child').click()`)
    await waitFor(textHas('Profile 已切换，已恢复自动选网'))
    assert.equal(await evaluate('document.querySelector(\'button[aria-label="启用 Singtel"]\').disabled'), true)

    await click('通知管理')
    await waitFor(textHas('通知 #28'))
    assert.equal(await evaluate(`document.querySelector('[data-notification="30"]').innerText.includes('install（安装）')`), true)
    assert.equal(await evaluate('document.querySelectorAll("[data-notification-group]").length'), 2)
    assert.deepEqual(await evaluate(`Array.from(document.querySelector('[data-notification-group="8965012512294623745"]').querySelectorAll('[data-notification]')).map(x => x.getAttribute('data-notification'))`), ['26', '28', '29', '30'])
    await click('折叠 ICCID 8965012512294623745 的通知')
    await waitFor(`document.querySelector('[data-notification-group="8965012512294623745"]').querySelectorAll('[data-notification]').length === 0`)
    assert.equal(await evaluate('document.querySelectorAll("[data-notification]").length'), 1)
    await click('展开 ICCID 8965012512294623745 的通知')
    await waitFor('document.querySelectorAll("[data-notification]").length === 5')
    await waitFor(`getComputedStyle(document.querySelector('[data-notification]').parentElement).gridTemplateColumns.split(' ').length === 2`)
    await call('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: false })
    await waitFor(`getComputedStyle(document.querySelector('[data-notification]').parentElement).gridTemplateColumns.split(' ').length === 1`)
    assert.equal(await evaluate(`Array.from(document.querySelectorAll('[data-notification-group]')).every(x => x.scrollWidth <= x.clientWidth)`), true)
    await waitFor(`(() => {
      const bulk = document.querySelector('[data-notification-bulk-actions]');
      const content = bulk.closest('.MuiDialogContent-root');
      const cards = Array.from(content.querySelectorAll('[data-notification]'));
      return cards.every(card => card.getBoundingClientRect().bottom <= bulk.getBoundingClientRect().top)
        && content.scrollHeight > content.clientHeight;
    })()`)
    await call('Emulation.clearDeviceMetricsOverride')
    await click('发送通知 28')
    await waitFor(textHas('通知已发送，记录保留供自行管理。'))
    assert.equal(await evaluate('document.querySelectorAll("[data-notification]").length'), 5)
    assert.equal(actions.at(-1)?.body.seq_number, 28)
    await click('删除通知 28')
    await waitFor(textHas('确认删除通知 #28'))
    assert.equal(actions.filter((item) => item.path.endsWith('/remove')).length, 0)
    await click('取消')
    await click('删除通知 29')
    await waitFor(textHas('future-operation 操作'))
    await click('取消')
    await click('删除通知 30')
    await waitFor(textHas('install 操作'))
    await click('取消')
    await evaluate(`document.querySelector('[data-notification-bulk-actions]').scrollIntoView({ block: 'end' })`)
    await click('全部删除')
    await waitFor(textHas('已删除 2 条通知。'))
    assert.equal(actions.at(-1)?.body.all, true)
    await waitFor('document.querySelectorAll("[data-notification]").length === 3')
    await click('删除通知 28')
    await waitFor(textHas('确认删除通知 #28'))
    await click('确认删除')
    await waitFor('document.querySelectorAll("[data-notification]").length === 2')
    assert.equal(actions.at(-1)?.body.confirm_sensitive, true)
    await click('全部发送')
    await waitFor(textHas('通知已发送，记录保留供自行管理。'))
    assert.equal(actions.at(-1)?.body.all, true)
    assert.equal(await evaluate('document.querySelectorAll("[data-notification]").length'), 2)
    await click('关闭')

    const currentDownload = (): ServerResponse | undefined => downloadResponse
    const startDownload = async (): Promise<ServerResponse> => {
      downloadResponse = undefined
      await click('下载 Profile')
      await waitFor('Array.from(document.querySelectorAll("label")).some(x => x.innerText.includes("LPA 激活码"))')
      await fill('LPA 激活码', 'LPA:1$rsp.truphone.com$test')
      await click('开始下载')
      await waitFor(textHas('正在下载您的 eSIM'))
      for (let attempt = 0; attempt < 100; attempt++) {
        const response = currentDownload()
        if (response) return response
        await delay(20)
      }
      assert.fail('Download request did not reach mock server')
    }
    const emit = (response: ServerResponse, message: string, data: unknown = null, type: LpacEvent['type'] = 'progress', code = 0) => {
      const event = { type, payload: { code, message, data } }
      response.write(`${JSON.stringify({ type: 'output', text: `${JSON.stringify(event)}\n` })}\n`)
      response.write(`${JSON.stringify({ type: 'lpac', event })}\n`)
      return event.payload
    }
    const connection = await startDownload()
    emit(connection, 'es10b_get_euicc_challenge_and_info')
    emit(connection, 'es9p_initiate_authentication')
    await waitFor(`document.querySelector('[aria-label="正在连接服务器：进行中"]') !== null`)
    assert.equal(await evaluate('document.getElementById("lpac-raw-output") === null'), true)
    emit(connection, 'es10b_cancel_session')
    emit(connection, 'es9p_cancel_session')
    const failure = emit(connection, 'es9p_initiate_authentication', 'HTTP transport failed', 'lpa', -1)
    connection.end(`${JSON.stringify({ type: 'complete', success: false, message: 'HTTP transport failed', result: failure })}\n`)
    await waitFor(textHas('无法与服务器正常通信'))
    assert.equal(await evaluate(`document.querySelector('[aria-label="正在连接服务器：失败"]') !== null`), true)
    await click('显示原始输出')
    await waitFor('document.getElementById("lpac-raw-output").innerText.includes("HTTP transport failed")')
    await click('关闭')

    const success = await startDownload()
    for (const message of ['es10b_get_euicc_challenge_and_info', 'es9p_initiate_authentication', 'es10b_authenticate_server', 'es9p_authenticate_client']) emit(success, message)
    emit(success, 'es8p_metadata_parse', { profileName: 'BetterRoaming', iccid: '8944476500020800251' })
    emit(success, 'es10b_prepare_download')
    emit(success, 'es9p_get_bound_profile_package')
    emit(success, 'es10b_load_bound_profile_package')
    await waitFor(`document.querySelector('[aria-label="正在写入 eSIM 配置文件：进行中"]') !== null`)
    assert.equal(await evaluate('document.querySelectorAll("[role=listitem][aria-label$=已完成]").length'), 4)
    assert.equal(await evaluate('document.getElementById("lpac-raw-output") === null'), true)
    const result = emit(success, 'success', { seqNumber: 30, iccid: '8944476500020800251' }, 'lpa')
    success.end(`${JSON.stringify({ type: 'complete', success: true, result })}\n`)
    await waitFor(textHas('配置文件已写入 eUICC'))
    assert.equal(await evaluate('document.querySelectorAll("[role=listitem][aria-label$=已完成]").length'), 5)
    await click('关闭')
  })
})
