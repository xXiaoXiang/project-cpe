/*
 * @Author: 1orz cloudorzi@gmail.com
 * @Date: 2025-12-10 10:16:39
 * @LastEditors: 1orz cloudorzi@gmail.com
 * @LastEditTime: 2025-12-13 12:44:31
 * @FilePath: /udx710-backend/frontend/src/pages/Dashboard/components/StatusOverview.tsx
 * @Description:
 *
 * Copyright (c) 2025 by 1orz, All Rights Reserved.
 */
import { Box, Chip, Tooltip, Typography, Paper, useTheme, type Theme } from '@mui/material'
import { alpha } from '@/utils/theme'
import {
  SignalCellularAlt,
  WifiTethering,
  Router,
  PowerSettingsNew,
  FlightTakeoff,
  TravelExplore,
} from '@mui/icons-material'
import { formatCarrierName, getCarrierColor, getCarrierInfo, getCarrierLogo } from '@/utils/carriers'
import { getSignalColor } from '../utils'
import type { DeviceInfo, NetworkInfo, CellsResponse, AirplaneModeResponse, ImsStatusResponse, RoamingResponse } from '@/api/types'

interface StatusOverviewProps {
  deviceInfo: DeviceInfo | null
  networkInfo: NetworkInfo | null
  cellsInfo: CellsResponse | null
  airplaneMode: AirplaneModeResponse | null
  imsStatus: ImsStatusResponse | null
  roaming?: RoamingResponse | null
  waitingMessage?: string | null
}

export function StatusOverview({
  deviceInfo,
  networkInfo,
  cellsInfo,
  airplaneMode,
  imsStatus,
  roaming,
  waitingMessage,
}: StatusOverviewProps) {
  const theme = useTheme<Theme>()
  const mode = airplaneMode ?? deviceInfo
  const radioOnline = mode?.powered === true && mode.online === true
  const flightMode = airplaneMode?.enabled === true && airplaneMode.powered
  const registered = radioOnline && ['registered', 'roaming'].includes(networkInfo?.registration_status ?? '')
  const searching = radioOnline && ['searching', 'initializing'].includes(networkInfo?.registration_status ?? '')

  const networkTech = (() => {
    if (!radioOnline) return 'N/A'
    if (networkInfo?.technology && networkInfo.technology !== 'unknown') return networkInfo.technology.toUpperCase()
    if (cellsInfo?.serving_cell?.tech) {
      return cellsInfo.serving_cell.tech.toUpperCase()
    }
    return 'N/A'
  })()

  const signalStrength = registered ? networkInfo?.signal_strength : undefined
  const signalColor = signalStrength === undefined ? 'text.disabled' : `${getSignalColor(signalStrength)}.main`
  const registrationLabel = !networkInfo
    ? '加载中'
    : networkInfo.registration_status === 'registered'
      ? '已注册'
      : networkInfo.registration_status === 'roaming'
        ? '漫游'
        : ({ searching: '正在搜网', initializing: '正在初始化', denied: '注册被拒绝', unregistered: '未注册', unknown: '未知' }[networkInfo.registration_status] || '未注册')
  const registrationColor = !networkInfo
    ? 'default'
    : networkInfo.registration_status === 'registered'
      ? 'success'
      : networkInfo.registration_status === 'roaming'
        ? 'warning'
        : 'default'
  const modemLabel = !mode ? '加载中' : flightMode ? '飞行模式' : !mode.powered ? 'Modem 未上电' : radioOnline ? '已上线' : '已离线'
  const modemColor = !mode ? 'default' : flightMode ? 'warning' : radioOnline ? 'success' : 'default'
  const carrierLabel = registered && networkInfo ? formatCarrierName(networkInfo.mcc, networkInfo.mnc)
    : !mode ? '载入中' : flightMode ? '射频已关闭' : !radioOnline ? '无网络连接' : registrationLabel
  const carrierLogo = registered && networkInfo ? getCarrierLogo(networkInfo.mcc, networkInfo.mnc) : null
  const carrierLogoBadge = registered && networkInfo ? getCarrierInfo(networkInfo.mcc, networkInfo.mnc)?.logoBadge : undefined

  return (
    <Paper
      data-network-overview
      elevation={0}
      sx={{
        p: 2,
        mb: 2,
        borderRadius: 2,
        background: (() => {
          const primaryMain = (theme.palette.primary as { main: string }).main
          const secondaryMain = (theme.palette.secondary as { main: string }).main
          return `linear-gradient(135deg, ${alpha(primaryMain, 0.08)} 0%, ${alpha(secondaryMain, 0.03)} 100%)`
        })(),
        border: (() => {
          const primaryMain = (theme.palette.primary as { main: string }).main
          return `1px solid ${alpha(primaryMain, 0.1)}`
        })(),
      }}
    >
      <Box display="flex" flexWrap="wrap" alignItems="center" gap={2}>
        <Box display="flex" alignItems="center" gap={1.5}>
          {carrierLogo ? (
            <Box sx={{ position: 'relative', display: 'inline-flex', pr: carrierLogoBadge ? 1 : 0 }}>
              <Box component="img" src={carrierLogo} alt={carrierLabel} title={carrierLabel} sx={{ height: 32, width: 'auto', maxWidth: 160, objectFit: 'contain' }} />
              {carrierLogoBadge && (
                <Box
                  component="span"
                  aria-hidden="true"
                  sx={{
                    position: 'absolute', right: 0, bottom: -2,
                    px: 0.4, borderRadius: 0.5, bgcolor: 'background.paper', color: 'text.primary',
                    fontSize: 10, fontWeight: 800, lineHeight: 1.4,
                    border: '1px solid', borderColor: 'divider',
                  }}
                >
                  {carrierLogoBadge}
                </Box>
              )}
            </Box>
          ) : !searching && (
            <Chip
              label={carrierLabel}
              color={registered && networkInfo ? getCarrierColor(networkInfo.mcc, networkInfo.mnc) : 'default'}
              size="small"
            />
          )}
          <Box display="flex" alignItems="center" gap={0.5}>
            <SignalCellularAlt sx={{ fontSize: 24, color: signalColor }} />
            <Typography variant="h6" fontWeight="bold" color={signalColor}>
              {signalStrength === undefined ? '--' : `${signalStrength}%`}
            </Typography>
          </Box>
        </Box>

        {(searching || networkTech !== 'N/A') && (
          <Tooltip title={searching ? registrationLabel : waitingMessage ?? ''}>
            <Box
              data-network-technology
              data-searching={searching}
              aria-label={searching ? `${networkTech === 'N/A' ? '-' : networkTech}，${registrationLabel}` : undefined}
              sx={{ position: 'relative', display: 'inline-flex', p: '3px', borderRadius: 999 }}
            >
              <Chip
                icon={<WifiTethering />}
                label={networkTech === 'N/A' ? '-' : networkTech}
                color={networkTech === '5G' || networkTech === 'NR' ? 'success' : 'primary'}
                size="small"
                sx={{ fontWeight: 'bold' }}
              />
              {searching && (
                <Box
                  component="svg"
                  aria-hidden="true"
                  sx={{
                    position: 'absolute', inset: 0, width: '100%', height: '100%',
                    pointerEvents: 'none', fill: 'none', color: 'primary.main',
                    '& rect': {
                      width: 'calc(100% - 2px)', height: 'calc(100% - 2px)',
                      stroke: 'currentColor', strokeWidth: 2, strokeLinecap: 'round',
                      strokeDasharray: '28 72',
                      animation: 'network-search-border 1.8s linear infinite',
                    },
                    '@keyframes network-search-border': {
                      from: { strokeDashoffset: 0 }, to: { strokeDashoffset: -100 },
                    },
                    '@media (prefers-reduced-motion: reduce)': {
                      '& rect': { animation: 'none' },
                    },
                  }}
                >
                  <rect x="1" y="1" rx="14" pathLength="100" />
                </Box>
              )}
            </Box>
          </Tooltip>
        )}

        {radioOnline && !searching && <Chip icon={<Router />} label={registrationLabel} color={registrationColor} variant="outlined" size="small" />}

        {registered && roaming?.is_roaming && (
          <Chip
            icon={<TravelExplore />}
            label={roaming.roaming_allowed ? '数据漫游已开启' : '数据漫游已关闭'}
            color={roaming.roaming_allowed ? 'info' : 'error'}
            size="small"
          />
        )}

        <Chip icon={flightMode ? <FlightTakeoff /> : <PowerSettingsNew />} label={modemLabel} color={modemColor} size="small" />

        {registered && imsStatus?.registered && <Chip label="VoLTE" color="info" size="small" variant="outlined" />}
      </Box>
    </Paper>
  )
}
