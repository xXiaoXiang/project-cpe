/*
 * @Author: 1orz cloudorzi@gmail.com
 * @Date: 2025-12-10 10:16:54
 * @LastEditors: 1orz cloudorzi@gmail.com
 * @LastEditTime: 2025-12-13 12:44:28
 * @FilePath: /udx710-backend/frontend/src/pages/Dashboard/components/QuickControls.tsx
 * @Description:
 *
 * Copyright (c) 2025 by 1orz, All Rights Reserved.
 */
import { Box, Card, CardContent, Typography, Stack, Switch, Chip, CircularProgress } from '@mui/material'
import { NetworkCheck, FlightTakeoff, TravelExplore } from '@mui/icons-material'
import type { AirplaneModeResponse, RoamingResponse } from '@/api/types'
import type { PendingControl } from '../hooks/useDashboardData'

interface QuickControlsProps {
  dataStatus: boolean | null
  airplaneMode: AirplaneModeResponse | null
  roaming: RoamingResponse | null
  pendingControl: PendingControl | null
  onToggleData: () => void
  onToggleAirplaneMode: () => void
  onToggleRoaming: () => void
}

export function QuickControls({
  dataStatus,
  airplaneMode,
  roaming,
  pendingControl,
  onToggleData,
  onToggleAirplaneMode,
  onToggleRoaming,
}: QuickControlsProps) {
  const progress = (control: PendingControl['control']) => pendingControl?.control === control && (
    <Box role="status" aria-live="polite" display="flex" alignItems="center" gap={0.5}>
      <CircularProgress size={14} />
      <Typography variant="caption" color="text.secondary">
        {pendingControl.target ? '正在开启…' : '正在关闭…'}
      </Typography>
    </Box>
  )
  return (
    <Card sx={{ height: '100%' }}>
      <CardContent>
        <Typography variant="subtitle2" color="text.secondary" gutterBottom>
          快捷控制
        </Typography>
        <Stack spacing={2}>
          <Box data-quick-control="data" aria-busy={pendingControl?.control === 'data'} display="flex" alignItems="center" justifyContent="space-between" flexWrap="wrap" gap={0.5}>
            <Box display="flex" alignItems="center" gap={1}>
              <NetworkCheck color={dataStatus ? 'success' : 'disabled'} />
              <Typography variant="body2">数据连接</Typography>
            </Box>
            {progress('data')}
            <Switch
              slotProps={{ input: { 'aria-label': '数据连接' } }}
              checked={dataStatus ?? false}
              onChange={onToggleData}
              color="success"
              size="small"
              disabled={dataStatus === null || !!pendingControl}
            />
          </Box>

          <Box data-quick-control="roaming" aria-busy={pendingControl?.control === 'roaming'} display="flex" alignItems="center" justifyContent="space-between" flexWrap="wrap" gap={0.5}>
            <Box display="flex" alignItems="center" gap={1}>
              <TravelExplore color={roaming?.roaming_allowed ? 'info' : 'disabled'} />
              <Typography variant="body2">数据漫游</Typography>
              {roaming?.is_roaming && (
                <Chip label="漫游中" size="small" color="warning" sx={{ height: 18, fontSize: '0.65rem' }} />
              )}
            </Box>
            {progress('roaming')}
            <Switch
              slotProps={{ input: { 'aria-label': '数据漫游' } }}
              checked={roaming?.roaming_allowed ?? false}
              onChange={onToggleRoaming}
              color="info"
              size="small"
              disabled={!roaming || !!pendingControl}
            />
          </Box>

          <Box data-quick-control="airplane" aria-busy={pendingControl?.control === 'airplane'} display="flex" alignItems="center" justifyContent="space-between" flexWrap="wrap" gap={0.5}>
            <Box display="flex" alignItems="center" gap={1}>
              <FlightTakeoff color={airplaneMode?.enabled ? 'warning' : 'disabled'} />
              <Typography variant="body2">飞行模式</Typography>
            </Box>
            {progress('airplane')}
            <Switch
              slotProps={{ input: { 'aria-label': '飞行模式' } }}
              checked={airplaneMode?.enabled ?? false}
              onChange={onToggleAirplaneMode}
              color="warning"
              size="small"
              disabled={!airplaneMode || !!pendingControl}
            />
          </Box>
        </Stack>
        {pendingControl?.control === 'airplane' && (
          <Typography variant="caption" color="text.secondary" sx={{ display: 'block', mt: 1 }}>
            正在等待设备确认，最多约 60 秒。
          </Typography>
        )}
      </CardContent>
    </Card>
  )
}
