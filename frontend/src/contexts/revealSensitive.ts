import { useState, useSyncExternalStore } from 'react'

let state = { revealAll: true, revision: 0 }
const listeners = new Set<() => void>()
const subscribe = (cb: () => void) => {
  listeners.add(cb)
  return () => { listeners.delete(cb) }
}

export const toggleRevealAll = () => {
  state = { revealAll: !state.revealAll, revision: state.revision + 1 }
  listeners.forEach((l) => l())
}

export const useRevealAll = () =>
  useSyncExternalStore(
    subscribe,
    () => state.revealAll,
  )

// Local hide/show still works with the global default. A global toggle resets
// card overrides so "hide all" always hides every card.
export function useSensitiveVisibility() {
  const global = useSyncExternalStore(subscribe, () => state)
  const [local, setLocal] = useState<{ revision: number; value: boolean } | null>(null)
  const visible = local?.revision === global.revision ? local.value : global.revealAll
  return [visible, (value: boolean) => setLocal({ revision: global.revision, value })] as const
}
