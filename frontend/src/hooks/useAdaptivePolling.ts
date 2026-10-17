import { useEffect, useMemo, useRef, useState } from 'react'

const DEFAULT_HIDDEN_MIN_INTERVAL = 30_000
const DEFAULT_HIDDEN_MULTIPLIER = 6

interface AdaptiveIntervalOptions {
  hiddenMinInterval?: number
  hiddenMultiplier?: number
}

interface AdaptivePollingOptions extends AdaptiveIntervalOptions {
  refreshInterval: number
  refreshKey?: number
  onTick: () => void | Promise<void>
  immediate?: boolean
}

export function usePageVisibility() {
  const [isPageVisible, setIsPageVisible] = useState(() =>
    typeof document === 'undefined' ? true : !document.hidden
  )

  useEffect(() => {
    if (typeof document === 'undefined') {
      return undefined
    }

    const handleVisibilityChange = () => {
      setIsPageVisible(!document.hidden)
    }

    document.addEventListener('visibilitychange', handleVisibilityChange)
    return () => {
      document.removeEventListener('visibilitychange', handleVisibilityChange)
    }
  }, [])

  return isPageVisible
}

export function getAdaptiveRefreshInterval(
  refreshInterval: number,
  isPageVisible: boolean,
  {
    hiddenMinInterval = DEFAULT_HIDDEN_MIN_INTERVAL,
    hiddenMultiplier = DEFAULT_HIDDEN_MULTIPLIER,
  }: AdaptiveIntervalOptions = {}
) {
  if (refreshInterval <= 0) {
    return 0
  }

  if (isPageVisible) {
    return refreshInterval
  }

  return Math.max(refreshInterval * hiddenMultiplier, hiddenMinInterval)
}

export function useAdaptivePolling({
  refreshInterval,
  refreshKey = 0,
  onTick,
  immediate = true,
  hiddenMinInterval,
  hiddenMultiplier,
}: AdaptivePollingOptions) {
  const tickRef = useRef(onTick)
  useEffect(() => { tickRef.current = onTick }, [onTick])
  const isPageVisible = usePageVisibility()
  const effectiveRefreshInterval = useMemo(
    () =>
      getAdaptiveRefreshInterval(refreshInterval, isPageVisible, {
        hiddenMinInterval,
        hiddenMultiplier,
      }),
    [hiddenMinInterval, hiddenMultiplier, isPageVisible, refreshInterval]
  )

  useEffect(() => {
    if (immediate) {
      void tickRef.current()
    }

    if (effectiveRefreshInterval <= 0) {
      return undefined
    }

    const timer = window.setInterval(() => {
      // Each task guards its own pending request; a slow endpoint must not block others.
      void tickRef.current()
    }, effectiveRefreshInterval)

    return () => {
      window.clearInterval(timer)
    }
  }, [effectiveRefreshInterval, immediate, refreshKey])

  return {
    effectiveRefreshInterval,
    isPageVisible,
  }
}

// Slow, cached resources use their own cadence, measured after completion.
// A deferred response may request an earlier retry without following the
// dashboard's 1/2/3/5/10-second refresh interval.
export function useCompletionPolling({
  interval,
  enabled,
  refreshKey,
  onTick,
}: {
  interval: number
  enabled: boolean
  refreshKey: number
  onTick: () => Promise<number | undefined>
}) {
  const tickRef = useRef(onTick)
  useEffect(() => { tickRef.current = onTick }, [onTick])
  const visible = usePageVisibility()
  useEffect(() => {
    let stopped = false
    let timer: number | undefined
    const tick = async () => {
      let retry: number | undefined
      try {
        retry = await tickRef.current()
      } catch {
        retry = interval
      } finally {
        if (!stopped && enabled) {
          timer = window.setTimeout(() => { void tick() },
            getAdaptiveRefreshInterval(retry ?? interval, visible))
        }
      }
    }
    void tick()
    return () => { stopped = true; window.clearTimeout(timer) }
  }, [enabled, interval, refreshKey, visible])
}
