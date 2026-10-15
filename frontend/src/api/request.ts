import { createSingleFlight } from '../utils/singleFlight.ts'

const pendingReads = createSingleFlight()

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
    return pendingReads(`${url}:${returnText ? 'text' : 'json'}:${options.cache ?? 'default'}`, performRequest)
  }
  return performRequest()
}
