import type { AirplaneModeResponse, ApiResponse, NetworkInfo } from './types.ts'

// Radio diagnostics and IMS wait for stable registration; SIM reads also work
// without registration. Other property reads remain independent of slow AT commands.
const diagnostics = new Set(['/cells', '/qos', '/location/cell-info', '/ims/status'])
const modemReads = new Set([
  ...diagnostics, '/sim', '/sim/slot', '/device/imeisv', '/data', '/roaming',
  '/radio-mode', '/band-lock', '/cell-lock', '/apn',
  '/network/operators', '/network/signal-strength', '/network/nitz',
])
const transitions = new Set([
  '/data', '/roaming',
  '/airplane-mode', '/sim/slot/switch', '/euicc/profile/enable', '/euicc/refresh-sim',
  '/radio-mode', '/network/register-manual', '/network/register-auto',
])

export function isModemTransition(url: string) { return transitions.has(url) }
export function isManagedModemRead(url: string) {
  return modemReads.has(url) || url === '/network' || url === '/airplane-mode'
}

export function isModemBusy(message: string) {
  return /org\.ofono\.Error\.InProgress|operation already in progress/i.test(message)
}

export function createModemReadPolicy(now = Date.now) {
  let airplane: AirplaneModeResponse | undefined
  let network: NetworkInfo | undefined
  let checkedAt = -Infinity
  let registeredAt = -Infinity
  let blockedUntil = 0
  let failures = 0
  let busyEndpoint: string | undefined
  let generation = 0
  let changing = 0
  let probe: Promise<void> | undefined
  let probeWaiting: ApiResponse<never> | undefined
  const endpointDelays = new Map<string, number>()

  const deferred = (message: string, retry = 5000): ApiResponse<never> => ({
    status: 'deferred', message, retry_after_ms: retry,
  })
  const transient = (url: string, result: ApiResponse<unknown>) => {
    if (isModemBusy(result.message ?? '')) {
      failures++
      busyEndpoint = url
      blockedUntil = now() + Math.min(30_000, 5000 * 2 ** Math.min(failures - 1, 3))
      return deferred('Modem 正忙，稍后自动重试', blockedUntil - now())
    }
    if (result.status === 'deferred') return deferred('Modem 状态正在恢复，稍后自动重试', result.retry_after_ms)
    // Older backends report a temporarily missing IMS interface as an error.
    // Back off this endpoint without hiding unrelated DBus/permission failures.
    if (url === '/ims/status' && /UnknownMethod|UnknownInterface/.test(result.message ?? '')
      && result.message?.includes('org.ofono.IpMultimediaSystem')) {
      return deferred('等待 IMS 接口恢复')
    }
    return undefined
  }
  const observe = (url: string, result: ApiResponse<unknown>) => {
    if (result.status !== 'ok' || !result.data) return
    if (url === '/airplane-mode') {
      airplane = result.data as AirplaneModeResponse
      if (!airplane.online || !airplane.powered) {
        network = undefined
        registeredAt = -Infinity
      }
    }
    if (url === '/network') {
      const next = result.data as NetworkInfo
      const registered = ['registered', 'roaming'].includes(next.registration_status)
      if (!registered) registeredAt = -Infinity
      else if (!network || !['registered', 'roaming'].includes(network.registration_status)) registeredAt = now()
      network = next
    }
  }

  return {
    beginTransition() {
      generation++
      changing++
      airplane = undefined
      network = undefined
      checkedAt = -Infinity
      registeredAt = -Infinity
      probeWaiting = undefined
      endpointDelays.clear()
    },
    endTransition() {
      changing = Math.max(0, changing - 1)
      generation++
      checkedAt = -Infinity
      blockedUntil = now() + 3000
    },
    async read<T>(url: string, read: (path: string) => Promise<ApiResponse<unknown>>): Promise<T> {
      const guarded = modemReads.has(url)
      if (!guarded && url !== '/network' && url !== '/airplane-mode') return await read(url) as T
      const version = generation
      if ((guarded || url === '/network') && (changing || now() < blockedUntil)) {
        return deferred('Modem 正在恢复，稍后自动重试', Math.max(1000, blockedUntil - now())) as T
      }
      if (now() < (endpointDelays.get(url) ?? 0)) return deferred('等待 Modem 接口恢复') as T

      if (guarded) {
        // Probe at most once every five seconds, shared across pages and endpoints.
        // A probe never waits for SIM/AT diagnostics, so slow reads cannot starve it.
        if (now() - checkedAt >= 5000) {
          if (!probe) {
            const pendingProbe = (async () => {
              const mode = await read('/airplane-mode')
              if (version !== generation) return
              observe('/airplane-mode', mode)
              probeWaiting = transient('/airplane-mode', mode)
              if (probeWaiting) { checkedAt = now(); return }
              if (mode.status !== 'ok') throw new Error(mode.message)
              if (airplane?.online && airplane.powered) {
                const registration = await read('/network')
                if (version !== generation) return
                observe('/network', registration)
                probeWaiting = transient('/network', registration)
                if (probeWaiting) { checkedAt = now(); return }
                if (registration.status !== 'ok') throw new Error(registration.message)
              }
              checkedAt = now()
            })()
            probe = pendingProbe
            void pendingProbe.finally(() => { if (probe === pendingProbe) probe = undefined }).catch(() => {})
          }
          try { await probe } catch (error) {
            // Genuine failures still reach the page; only modem-busy is deferred.
            if (!isModemBusy(String(error))) throw error
            failures++
            blockedUntil = now() + Math.min(30_000, 5000 * 2 ** Math.min(failures - 1, 3))
            return deferred('Modem 正忙，稍后自动重试', blockedUntil - now()) as T
          }
        }
        if (probeWaiting) return probeWaiting as T
        if (version !== generation || changing || now() < blockedUntil) return deferred('Modem 正在恢复') as T
        if (!airplane?.powered || !airplane.online) return deferred('射频已关闭，暂停 Modem 查询') as T
        if (diagnostics.has(url)) {
          if (!network || !['registered', 'roaming'].includes(network.registration_status)
            || now() - registeredAt < 3000) return deferred('正在搜网，等待网络注册稳定') as T
        }
      }

      const result = await read(url)
      // Discard responses started before a SIM/radio change, even if they finish later.
      if (version !== generation) return deferred('Modem 状态已变化，等待重新读取') as T
      observe(url, result)
      const waiting = transient(url, result)
      if (waiting) {
        endpointDelays.set(url, now() + Math.max(5000, waiting.retry_after_ms ?? 5000))
        return waiting as T
      }
      if (guarded && result.status === 'ok') {
        if (url === busyEndpoint) { failures = 0; busyEndpoint = undefined }
        endpointDelays.delete(url)
      }
      return result as T
    },
  }
}
