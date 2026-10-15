import { useState } from 'react'
import { createSingleFlight } from '../utils/singleFlight'

export function useSingleFlight() {
  const [run] = useState(createSingleFlight)
  return run
}
