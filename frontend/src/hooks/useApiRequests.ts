import { useCallback, useEffect, useRef, useState } from 'react'
import type { ApiResponse } from '../api/types'
import { useSingleFlight } from './useSingleFlight'

// Each response updates its own section, even when another endpoint is still pending.
export function useApiRequests() {
  const run = useSingleFlight()
  const active = useRef(true)
  const errors = useRef(new Map<string, string>())
  const [initialLoading, setInitialLoading] = useState(true)
  const [requestError, setRequestError] = useState<string | null>(null)
  const [manualError, setManualError] = useState<string | null>(null)
  const setError = useCallback((message: string | null) => {
    setManualError(message)
    if (message === null) setRequestError(null)
  }, [])
  const [ready, setReady] = useState<Record<string, boolean>>({})
  const [waiting, setWaiting] = useState<Record<string, string>>({})

  useEffect(() => {
    active.current = true
    return () => { active.current = false }
  }, [])

  const fetchInto = useCallback(<T,>(
    key: string,
    label: string,
    request: () => Promise<ApiResponse<T>>,
    apply: (data: T) => void,
    onDeferred?: () => void,
  ) => run(key, async () => {
    try {
      const response = await request()
      if (response.status === 'deferred') {
        errors.current.delete(key)
        if (active.current) {
          onDeferred?.()
          setWaiting((current) => current[key] === response.message ? current : { ...current, [key]: response.message })
        }
        return
      }
      if (response.status !== 'ok' || response.data === undefined || response.data === null) {
        throw new Error(response.message || '未返回有效数据')
      }
      if (!active.current) return
      apply(response.data)
      setWaiting((current) => {
        if (!(key in current)) return current
        const next = { ...current }
        delete next[key]
        return next
      })
      setReady((current) => current[key] ? current : { ...current, [key]: true })
      errors.current.delete(key)
    } catch (reason) {
      if (!active.current) return
      errors.current.set(key, `${label}: ${reason instanceof Error ? reason.message : String(reason)}`)
    } finally {
      if (active.current) {
        setInitialLoading(false)
        // A successful stats tick must not erase an interactive write failure.
        setRequestError(errors.current.values().next().value ?? null)
      }
    }
  }), [run])

  return { fetchInto, initialLoading, error: manualError ?? requestError, setError, ready, waiting, waitingMessage: Object.values(waiting)[0] ?? null }
}
