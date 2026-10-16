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
import { useApiRequests } from '@/hooks/useApiRequests'
import {
  getAdaptiveRefreshInterval,
  useAdaptivePolling,
  usePageVisibility,
} from '@/hooks/useAdaptivePolling'
import type {
  DeviceInfo,
  NetworkInfo,
  CellsResponse,
  QosInfo,
  SimInfo,
  SystemStatsResponse,
  AirplaneModeResponse,
  ImsStatusResponse,
  RoamingResponse,
  ApiResponse,
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

export interface PendingControl {
  control: 'data' | 'airplane' | 'roaming'
  target: boolean
}

const CONNECTIVITY_MIN_REFRESH_INTERVAL = 15_000
const CONNECTIVITY_HIDDEN_MIN_REFRESH_INTERVAL = 60_000

export function useDashboardData(refreshInterval: number, refreshKey: number) {
  const { initialLoading, error, setError, fetchInto, waitingMessage } = useApiRequests()

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
  const [pendingControl, setPendingControl] = useState<PendingControl | null>(null)
  const pendingControlRef = useRef<PendingControl | null>(null)
  const controlRevision = useRef(0)

  const [speedHistory, setSpeedHistory] = useState<Record<string, InterfaceSpeedHistory>>({})
  const speedHistoryRef = useRef<Record<string, InterfaceSpeedHistory>>({})

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

  const isPageVisible = usePageVisibility()
  const connectivityRefreshInterval = getAdaptiveRefreshInterval(
    refreshInterval > 0 ? Math.max(refreshInterval, CONNECTIVITY_MIN_REFRESH_INTERVAL) : 0,
    isPageVisible,
    {
      hiddenMinInterval: CONNECTIVITY_HIDDEN_MIN_REFRESH_INTERVAL,
      hiddenMultiplier: 4,
    }
  )

  const loadData = useCallback(async () => {
    const revision = controlRevision.current
    const applyControl = <T,>(apply: (data: T) => void) => (data: T) => {
      // A polling response started before/during a write must not overwrite its
      // confirmed result, even if the response body finishes after the write.
      if (!pendingControlRef.current && revision === controlRevision.current) apply(data)
    }
    await Promise.allSettled([
      fetchInto('device', '设备信息', () => api.getDeviceInfo(), setDeviceInfo),
      fetchInto('sim', 'SIM 信息', () => api.getSimInfo(), setSimInfo, () => setSimInfo(null)),
      fetchInto('stats', '系统状态', () => api.getSystemStats(), (stats) => {
        setSystemStats(stats)
        updateSpeedHistory(stats)
      }),
      fetchInto('network', '网络信息', () => api.getNetworkInfo(), setNetworkInfo, () => setNetworkInfo(null)),
      pendingControlRef.current ? Promise.resolve() : fetchInto('data', '数据连接状态', () => api.getDataStatus(), applyControl((status) => setDataStatus(status.active))),
      fetchInto('cells', '小区信息', () => api.getCellsInfo(), setCellsInfo, () => setCellsInfo(null)),
      fetchInto('qos', 'QoS 信息', () => api.getQosInfo(), setQosInfo, () => setQosInfo(null)),
      pendingControlRef.current ? Promise.resolve() : fetchInto('airplane', '飞行模式状态', () => api.getAirplaneMode(), applyControl(setAirplaneMode)),
      fetchInto('ims', 'IMS 状态', () => api.getImsStatus(), setImsStatus, () => setImsStatus(null)),
      pendingControlRef.current ? Promise.resolve() : fetchInto('roaming', '漫游状态', () => api.getRoamingStatus(), applyControl(setRoaming)),
    ])
  }, [fetchInto, updateSpeedHistory])

  const loadConnectivity = useCallback(async () => {
    await fetchInto('connectivity', '连通性检测', () => api.getConnectivity(), setConnectivity)
  }, [fetchInto])

  const changeControl = useCallback(async <T,>(
    control: PendingControl['control'],
    target: boolean,
    request: () => Promise<ApiResponse<T>>,
    apply: (data: T) => void,
  ) => {
    if (pendingControlRef.current) return
    const pending = { control, target }
    pendingControlRef.current = pending
    controlRevision.current++
    setPendingControl(pending)
    setError(null)
    try {
      const response = await request()
      if (response.status !== 'ok' || response.data === undefined || response.data === null) {
        throw new Error(response.message || '设备未确认操作结果，请重试')
      }
      apply(response.data)
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    } finally {
      controlRevision.current++
      pendingControlRef.current = null
      setPendingControl(null)
    }
  }, [setError])

  const toggleData = useCallback(async () => {
    if (dataStatus === null) return
    await changeControl('data', !dataStatus, () => api.setDataStatus(!dataStatus), (status) => setDataStatus(status.active))
  }, [dataStatus, changeControl])

  const toggleAirplaneMode = useCallback(async () => {
    if (!airplaneMode) return
    await changeControl('airplane', !airplaneMode.enabled, () => api.setAirplaneMode(!airplaneMode.enabled), setAirplaneMode)
  }, [airplaneMode, changeControl])

  const toggleRoaming = useCallback(async () => {
    if (!roaming) return
    await changeControl('roaming', !roaming.roaming_allowed, () => api.setRoamingAllowed(!roaming.roaming_allowed), setRoaming)
  }, [roaming, changeControl])

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
    waitingMessage,
    pendingControl,
    data: {
      deviceInfo,
      simInfo,
      systemStats,
      networkInfo,
      dataStatus,
      cellsInfo: airplaneMode?.enabled || !['registered', 'roaming'].includes(networkInfo?.registration_status ?? '') ? null : cellsInfo,
      qosInfo: airplaneMode?.enabled || !['registered', 'roaming'].includes(networkInfo?.registration_status ?? '') ? null : qosInfo,
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
