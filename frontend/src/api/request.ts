import { createSingleFlight } from '../utils/singleFlight.ts'
import type { ApiResponse } from './types.ts'
import { createModemReadPolicy, isManagedModemRead, isModemTransition } from './modemPolling.ts'

const pendingReads = createSingleFlight()
const policy = createModemReadPolicy()
const rawReads = createSingleFlight()

export function request<T>(
  url: string,
  options: RequestInit & { returnText?: boolean } = {},
): Promise<T> {
  const { returnText, ...fetchOptions } = options
  const performRequest = async (): Promise<T> => {
    const response = await fetch(`/api${url}`, {
      ...fetchOptions,
      headers: {
        'Content-Type': 'application/json',
        ...fetchOptions.headers,
      },
    })
    if (!response.ok) throw new Error(`HTTP error! status: ${response.status}`)
    return (returnText ? await response.text() : await response.json()) as T
  }

  // Reuse reads across timers, manual refreshes and route remounts. Never cancel
  // a pending read or retry it just because a refresh interval has elapsed.
  // Writes and requests with caller-owned cancellation remain independent.
  if ((options.method ?? 'GET').toUpperCase() === 'GET'
    && !options.signal && !options.body && !options.headers) {
    return pendingReads(`${url}:${returnText ? 'text' : 'json'}:${options.cache ?? 'default'}`, () => {
      if (returnText || !isManagedModemRead(url)) return performRequest()
      return policy.read<T>(url, (path) => rawReads(`${path}:${options.cache ?? 'default'}`, async () => {
        if (path === url) return await performRequest() as ApiResponse<unknown>
        const response = await fetch(`/api${path}`, { headers: { 'Content-Type': 'application/json' } })
        if (!response.ok) throw new Error(`HTTP error! status: ${response.status}`)
        return await response.json() as ApiResponse<unknown>
      }))
    })
  }
  if ((options.method ?? 'GET').toUpperCase() !== 'GET' && isModemTransition(url)) {
    policy.beginTransition()
    return performRequest().finally(() => policy.endTransition())
  }
  return performRequest()
}
