import { useCallback, useEffect, useState } from 'react'
import {
  Alert, Box, Button, Card, CardContent, CircularProgress, Dialog, DialogActions,
  DialogContent, DialogContentText, DialogTitle, IconButton, Stack, Tooltip, Typography,
} from '@mui/material'
import { DeleteOutline, Refresh, Send } from '@mui/icons-material'
import { api } from '@/api'
import type { EuiccNotification, EuiccNotificationTarget } from '@/api/types'
import { useSingleFlight } from '../hooks/useSingleFlight'

const safeToRemove = (notification: EuiccNotification) =>
  ['enable', 'disable'].includes(notification.profileManagementOperation)
const operationLabels: Record<string, string> = { enable: '（启用）', disable: '（停用）', delete: '（删除）' }

export default function EuiccNotificationsDialog({ onClose }: { onClose: () => void }) {
  const [notifications, setNotifications] = useState<EuiccNotification[]>([])
  const [loading, setLoading] = useState(true)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [busy, setBusy] = useState<string | null>(null)
  const [feedback, setFeedback] = useState<{ severity: 'success' | 'error' | 'warning'; text: string } | null>(null)
  const [pendingRemoval, setPendingRemoval] = useState<EuiccNotification | null>(null)
  const run = useSingleFlight()
  const load = useCallback(() => run('notifications', async () => {
    setLoading(true)
    setLoadError(null)
    try {
      const response = await api.getEuiccNotifications()
      if (response.status !== 'ok' || !response.data) throw new Error(response.message || '读取通知失败')
      setNotifications(response.data)
    } catch (error) {
      setLoadError(error instanceof Error ? error.message : String(error))
    } finally {
      setLoading(false)
    }
  }), [run])

  useEffect(() => { void load() }, [load])

  const act = async (operation: 'process' | 'remove', target: EuiccNotificationTarget) => {
    if (busy) return
    setBusy(`${operation}:${target.all ? 'all' : target.seq_number}`)
    setFeedback(null)
    try {
      if (operation === 'process') {
        const response = await api.processEuiccNotifications(target)
        if (response.status !== 'ok') throw new Error(response.message || '通知发送失败')
        setFeedback({ severity: 'success', text: '通知已发送，记录保留供自行管理。' })
      } else {
        const response = await api.removeEuiccNotifications(target)
        if (response.status !== 'ok' || !response.data) throw new Error(response.message || '通知删除失败')
        const { removed, failed, skipped } = response.data
        setFeedback({
          severity: failed.length ? 'error' : skipped ? 'warning' : 'success',
          text: `已删除 ${removed.length} 条通知。${skipped ? `保留 ${skipped} 条其他操作通知。` : ''}${failed.length
            ? `删除失败：${failed.map((item) => `#${item.seqNumber} ${item.message}`).join('；')}` : ''}`,
        })
      }
    } catch (error) {
      setFeedback({ severity: 'error', text: error instanceof Error ? error.message : String(error) })
    } finally {
      setPendingRemoval(null)
      // Sending does not imply removal: always display the actual list returned by LPAC.
      await load()
      setBusy(null)
    }
  }

  const disabled = loading || busy !== null
  const groups = new Map<string, EuiccNotification[]>()
  for (const notification of [...notifications].sort((a, b) => a.seqNumber - b.seqNumber)) {
    const group = groups.get(notification.iccid) ?? []
    group.push(notification)
    groups.set(notification.iccid, group)
  }
  return (
    <>
      <Dialog open onClose={() => !busy && onClose()} fullWidth maxWidth="sm"
        slotProps={{ paper: { sx: { width: 500, height: 'min(80vh, 760px)' } } }}>
        <DialogTitle component="div">
          <Stack direction="row" alignItems="center" justifyContent="space-between" gap={1} flexWrap="wrap">
            <Typography component="h2" variant="h6">通知管理</Typography>
            <Stack direction="row" spacing={0.5}>
              <Button size="small" disabled={disabled || !notifications.length}
                onClick={() => void act('process', { all: true })}>
                {busy === 'process:all' && <CircularProgress size={14} sx={{ mr: 0.5 }} />}全部发送
              </Button>
              <Button size="small" color="error" disabled={disabled || !notifications.some(safeToRemove)}
                onClick={() => void act('remove', { all: true })}>
                {busy === 'remove:all' && <CircularProgress size={14} sx={{ mr: 0.5 }} />}全部删除
              </Button>
            </Stack>
          </Stack>
        </DialogTitle>
        <DialogContent dividers>
          <Stack spacing={1.5}>
            <Typography variant="body2" color="text.secondary">
              发送后保留通知记录。“全部删除”仅删除 enable / disable 通知。
            </Typography>
            {feedback && <Alert severity={feedback.severity} sx={{ overflowWrap: 'anywhere' }}>{feedback.text}</Alert>}
            {loadError && <Alert severity="error">{loadError}</Alert>}
            {loading && <Box sx={{ textAlign: 'center', py: 2 }}><CircularProgress size={24} aria-label="正在读取通知" /></Box>}
            {!loading && !loadError && !notifications.length && <Alert severity="info">没有待处理的通知。</Alert>}
            {Array.from(groups, ([iccid, items]) => (
              <Box component="section" key={iccid} data-notification-group={iccid} aria-label={`ICCID ${iccid}`}
                sx={{ border: 1, borderColor: 'divider', borderRadius: 1.5, p: 1.5, bgcolor: 'action.hover' }}>
                <Typography variant="caption" color="text.secondary">ICCID</Typography>
                <Typography variant="body2" sx={{ fontFamily: 'monospace', overflowWrap: 'anywhere', mb: 1.5 }}>{iccid}</Typography>
                <Stack spacing={1}>
                  {items.map((notification) => (
              <Card key={notification.seqNumber} variant="outlined" data-notification={notification.seqNumber}>
                <CardContent sx={{ display: 'flex', gap: 1.5, alignItems: 'center', '&:last-child': { pb: 2 } }}>
                  <Stack spacing={0.75} sx={{ flex: 1, minWidth: 0 }}>
                    <Typography fontWeight={600}>通知 #{notification.seqNumber}</Typography>
                    <Typography variant="body2" sx={{ overflowWrap: 'anywhere' }}>
                      操作：{notification.profileManagementOperation}
                      {operationLabels[notification.profileManagementOperation] || ''}
                    </Typography>
                  </Stack>
                  <Stack spacing={1}>
                    <Tooltip title="发送通知">
                      <span><IconButton aria-label={`发送通知 ${notification.seqNumber}`} color="primary" disabled={disabled}
                        onClick={() => void act('process', { seq_number: notification.seqNumber })}>
                        {busy === `process:${notification.seqNumber}` ? <CircularProgress size={20} /> : <Send />}
                      </IconButton></span>
                    </Tooltip>
                    <Tooltip title="删除通知">
                      <span><IconButton aria-label={`删除通知 ${notification.seqNumber}`} color="error" disabled={disabled}
                        onClick={() => safeToRemove(notification)
                          ? void act('remove', { seq_number: notification.seqNumber }) : setPendingRemoval(notification)}>
                        {busy === `remove:${notification.seqNumber}` ? <CircularProgress size={20} /> : <DeleteOutline />}
                      </IconButton></span>
                    </Tooltip>
                  </Stack>
                </CardContent>
              </Card>
                  ))}
                </Stack>
              </Box>
            ))}
          </Stack>
        </DialogContent>
        <DialogActions>
          <Button startIcon={<Refresh />} disabled={disabled} onClick={() => void load()}>刷新</Button>
          <Button disabled={busy !== null} onClick={onClose}>关闭</Button>
        </DialogActions>
      </Dialog>
      <Dialog open={pendingRemoval !== null} onClose={() => !busy && setPendingRemoval(null)} maxWidth="xs">
        <DialogTitle>确认删除通知 #{pendingRemoval?.seqNumber}</DialogTitle>
        <DialogContent>
          <DialogContentText>
            此通知为向运营商发送配置文件的 {pendingRemoval?.profileManagementOperation} 操作，删除后可能会导致此
            Profile 无法再次下载或者激活，请谨慎处理。确认要删除此项通知吗？
          </DialogContentText>
        </DialogContent>
        <DialogActions>
          <Button disabled={busy !== null} onClick={() => setPendingRemoval(null)}>取消</Button>
          <Button variant="contained" color="error" disabled={busy !== null}
            startIcon={busy ? <CircularProgress size={16} color="inherit" /> : undefined}
            onClick={() => pendingRemoval && void act('remove', { seq_number: pendingRemoval.seqNumber, confirm_sensitive: true })}>
            确认删除
          </Button>
        </DialogActions>
      </Dialog>
    </>
  )
}
