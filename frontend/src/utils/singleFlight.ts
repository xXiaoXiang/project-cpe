// Reuse the pending task for a key. Other keys can finish and refresh independently.
export function createSingleFlight() {
  const pending = new Map<string, Promise<unknown>>()

  return function run<T>(key: string, task: () => Promise<T>): Promise<T> {
    const existing = pending.get(key)
    if (existing) return existing as Promise<T>

    const result = Promise.resolve().then(task).finally(() => {
      pending.delete(key)
    })
    pending.set(key, result)
    return result
  }
}
