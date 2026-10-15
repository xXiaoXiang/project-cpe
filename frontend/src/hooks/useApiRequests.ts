import { useCallback, useEffect, useRef, useState } from 'react'
import type { ApiResponse } from '../api/types'
import { useSingleFlight } from './useSingleFlight'

// Each response updates its own section, even when another endpoint is still pending.
export function useApiRequests() {
  const run = useSingleFlight()
  const active = useRef(true)
  const errors = useRef(new Map<string, string>())
  const [initialLoading, setInitialLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [ready, setReady] = useState<Record<string, boolean>>({})

  useEffect(() => {
    active.current = true
    return () => { active.current = false }
  }, [])

  const fetchInto = useCallback(<T,>(
    key: string,
    label: string,
    request: () => Promise<ApiResponse<T>>,
    apply: (data: T) => void,
  ) => run(key, async () => {
    try {
      const response = await request()
      if (response.status !== 'ok' || response.data === undefined || response.data === null) {
        throw new Error(response.message || '未返回有效数据')
      }
      if (!active.current) return
      apply(response.data)
      setReady((current) => current[key] ? current : { ...current, [key]: true })
      errors.current.delete(key)
    } catch (reason) {
      if (!active.current) return
      errors.current.set(key, `${label}: ${reason instanceof Error ? reason.message : String(reason)}`)
    } finally {
      if (active.current) {
        setInitialLoading(false)
        setError(errors.current.values().next().value ?? null)
      }
    }
  }), [run])

  return { fetchInto, initialLoading, error, setError, ready }
}
