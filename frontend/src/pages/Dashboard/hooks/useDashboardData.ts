/*
 * @Author: 1orz cloudorzi@gmail.com
 * @Date: 2025-12-10 10:15:57
 * @LastEditors: 1orz cloudorzi@gmail.com
 * @LastEditTime: 2025-12-13 12:44:40
 * @FilePath: /udx710-backend/frontend/src/pages/Dashboard/hooks/useDashboardData.ts
 * @Description:
 *
 * Copyright (c) 2025 by 1orz, All Rights Reserved.
 */
import { useState, useCallback, useRef } from 'react'
import { api } from '@/api'
import {
  getAdaptiveRefreshInterval,
  useAdaptivePolling,
  usePageVisibility,
} from '@/hooks/useAdaptivePolling'
import type {
  ApiResponse,
  DeviceInfo,
  NetworkInfo,
  CellsResponse,
  QosInfo,
  SimInfo,
  SystemStatsResponse,
  AirplaneModeResponse,
  ImsStatusResponse,
  RoamingResponse,
} from '@/api/types'

export const SPEED_HISTORY_MAX_POINTS = 30

export interface InterfaceSpeedHistory {
  rx: number[]
  tx: number[]
  totalRx: number
  totalTx: number
}

export interface ConnectivityResult {
  ipv4: { success: boolean; latency_ms?: number }
  ipv6: { success: boolean; latency_ms?: number }
}

export interface DashboardData {
  deviceInfo: DeviceInfo | null
  simInfo: SimInfo | null
  systemStats: SystemStatsResponse | null
  networkInfo: NetworkInfo | null
  dataStatus: boolean | null
  cellsInfo: CellsResponse | null
  qosInfo: QosInfo | null
  airplaneMode: AirplaneModeResponse | null
  imsStatus: ImsStatusResponse | null
  connectivity: ConnectivityResult | null
  speedHistory: Record<string, InterfaceSpeedHistory>
  roaming: RoamingResponse | null
}

export interface DashboardActions {
  toggleData: () => Promise<void>
  toggleAirplaneMode: () => Promise<void>
  toggleRoaming: () => Promise<void>
  loadData: () => Promise<void>
}

const CONNECTIVITY_MIN_REFRESH_INTERVAL = 15_000
const CONNECTIVITY_HIDDEN_MIN_REFRESH_INTERVAL = 60_000

export function useDashboardData(refreshInterval: number, refreshKey: number) {
  const [initialLoading, setInitialLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const [deviceInfo, setDeviceInfo] = useState<DeviceInfo | null>(null)
  const [simInfo, setSimInfo] = useState<SimInfo | null>(null)
  const [systemStats, setSystemStats] = useState<SystemStatsResponse | null>(null)
  const [networkInfo, setNetworkInfo] = useState<NetworkInfo | null>(null)
  const [dataStatus, setDataStatus] = useState<boolean | null>(null)
  const [cellsInfo, setCellsInfo] = useState<CellsResponse | null>(null)
  const [qosInfo, setQosInfo] = useState<QosInfo | null>(null)
  const [airplaneMode, setAirplaneMode] = useState<AirplaneModeResponse | null>(null)
  const [imsStatus, setImsStatus] = useState<ImsStatusResponse | null>(null)
  const [connectivity, setConnectivity] = useState<ConnectivityResult | null>(null)
  const [roaming, setRoaming] = useState<RoamingResponse | null>(null)

  const [speedHistory, setSpeedHistory] = useState<Record<string, InterfaceSpeedHistory>>({})
  const speedHistoryRef = useRef<Record<string, InterfaceSpeedHistory>>({})
  const inFlightRef = useRef(new Set<string>())
  const errorsRef = useRef(new Map<string, string>())

  const updateSpeedHistory = useCallback((stats: SystemStatsResponse | null) => {
    if (!stats?.network_speed?.interfaces) return

    const newHistory = { ...speedHistoryRef.current }

    for (const iface of stats.network_speed.interfaces) {
      const existing = newHistory[iface.interface] || { rx: [], tx: [], totalRx: 0, totalTx: 0 }

      const rxHistory = [...existing.rx, iface.rx_bytes_per_sec]
      const txHistory = [...existing.tx, iface.tx_bytes_per_sec]

      if (rxHistory.length > SPEED_HISTORY_MAX_POINTS) {
        rxHistory.shift()
        txHistory.shift()
      }

      newHistory[iface.interface] = {
        rx: rxHistory,
        tx: txHistory,
        totalRx: iface.total_rx_bytes,
        totalTx: iface.total_tx_bytes,
      }
    }

    speedHistoryRef.current = newHistory
    setSpeedHistory(newHistory)
  }, [])

  const formatRequestError = useCallback((label: string, errorValue: unknown) => {
    const message = errorValue instanceof Error ? errorValue.message : String(errorValue)
    return `${label}: ${message}`
  }, [])

  const isPageVisible = usePageVisibility()
  const connectivityRefreshInterval = getAdaptiveRefreshInterval(
    Math.max(refreshInterval, CONNECTIVITY_MIN_REFRESH_INTERVAL),
    isPageVisible,
    {
      hiddenMinInterval: CONNECTIVITY_HIDDEN_MIN_REFRESH_INTERVAL,
      hiddenMultiplier: 4,
    }
  )

  // 每个接口独立更新；同一接口上一次请求未返回时跳过本轮，避免慢接口堆积请求
  const fetchInto = useCallback(
    async <T,>(
      key: string,
      label: string,
      request: () => Promise<ApiResponse<T>>,
      apply: (data: T) => void
    ) => {
      if (inFlightRef.current.has(key)) return
      inFlightRef.current.add(key)
      try {
        const response = await request()
        if (response.data) apply(response.data)
        errorsRef.current.delete(key)
      } catch (errorValue) {
        errorsRef.current.set(key, formatRequestError(label, errorValue))
      } finally {
        inFlightRef.current.delete(key)
        setInitialLoading(false)
        setError(errorsRef.current.values().next().value ?? null)
      }
    },
    [formatRequestError]
  )

  const loadData = useCallback(async () => {
    await Promise.allSettled([
      fetchInto('device', '设备信息', () => api.getDeviceInfo(), setDeviceInfo),
      fetchInto('sim', 'SIM 信息', () => api.getSimInfo(), setSimInfo),
      fetchInto('stats', '系统状态', () => api.getSystemStats(), (stats) => {
        setSystemStats(stats)
        updateSpeedHistory(stats)
      }),
      fetchInto('network', '网络信息', () => api.getNetworkInfo(), setNetworkInfo),
      fetchInto('data', '数据连接状态', () => api.getDataStatus(), (status) => setDataStatus(status.active)),
      fetchInto('cells', '小区信息', () => api.getCellsInfo(), setCellsInfo),
      fetchInto('qos', 'QoS 信息', () => api.getQosInfo(), setQosInfo),
      fetchInto('airplane', '飞行模式状态', () => api.getAirplaneMode(), setAirplaneMode),
      fetchInto('ims', 'IMS 状态', () => api.getImsStatus(), setImsStatus),
      fetchInto('roaming', '漫游状态', () => api.getRoamingStatus(), setRoaming),
    ])
  }, [fetchInto, updateSpeedHistory])

  const loadConnectivity = useCallback(async () => {
    await fetchInto('connectivity', '连通性检测', () => api.getConnectivity(), setConnectivity)
  }, [fetchInto])

  const toggleData = useCallback(async () => {
    if (dataStatus === null) return

    try {
      const newStatus = !dataStatus
      await api.setDataStatus(newStatus)
      setDataStatus(newStatus)
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    }
  }, [dataStatus])

  const toggleAirplaneMode = useCallback(async () => {
    if (!airplaneMode) return

    try {
      const newEnabled = !airplaneMode.enabled
      const response = await api.setAirplaneMode(newEnabled)
      if (response.data) {
        setAirplaneMode(response.data)
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    }
  }, [airplaneMode])

  const toggleRoaming = useCallback(async () => {
    if (!roaming) return

    try {
      const newAllowed = !roaming.roaming_allowed
      const response = await api.setRoamingAllowed(newAllowed)
      if (response.data) {
        setRoaming(response.data)
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    }
  }, [roaming])

  useAdaptivePolling({
    refreshInterval,
    refreshKey,
    onTick: loadData,
  })

  useAdaptivePolling({
    refreshInterval: connectivityRefreshInterval,
    refreshKey,
    onTick: loadConnectivity,
  })

  return {
    initialLoading,
    error,
    setError,
    data: {
      deviceInfo,
      simInfo,
      systemStats,
      networkInfo,
      dataStatus,
      cellsInfo,
      qosInfo,
      airplaneMode,
      imsStatus,
      connectivity,
      speedHistory,
      roaming,
    } as DashboardData,
    actions: {
      toggleData,
      toggleAirplaneMode,
      toggleRoaming,
      loadData,
    } as DashboardActions,
  }
}
