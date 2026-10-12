import { useSyncExternalStore } from 'react'

let revealAll = false
const listeners = new Set<() => void>()

export const toggleRevealAll = () => {
  revealAll = !revealAll
  listeners.forEach((l) => l())
}

export const useRevealAll = () =>
  useSyncExternalStore(
    (cb) => {
      listeners.add(cb)
      return () => listeners.delete(cb)
    },
    () => revealAll,
  )
