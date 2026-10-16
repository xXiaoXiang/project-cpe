import { useCallback, useEffect, useState } from 'react'
import {
  Alert, Box, Button, ButtonBase, Card, CardActions, CardContent, CircularProgress, Collapse,
  Dialog, DialogActions, DialogContent, DialogContentText, DialogTitle, Stack, Typography,
} from '@mui/material'
import { DeleteOutline, ExpandMore, Refresh, Send } from '@mui/icons-material'
import { api } from '@/api'
import type { EuiccNotification, EuiccNotificationTarget } from '@/api/types'
import { useSingleFlight } from '../hooks/useSingleFlight'

const safeToRemove = (notification: EuiccNotification) =>
  ['enable', 'disable'].includes(notification.profileManagementOperation)
const operationLabels: Record<string, string> = { enable: '（启用）', disable: '（停用）', delete: '（删除）', install: '（安装）' }

export default function EuiccNotificationsDialog({ onClose }: { onClose: () => void }) {
  const [notifications, setNotifications] = useState<EuiccNotification[]>([])
  const [loading, setLoading] = useState(true)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [busy, setBusy] = useState<string | null>(null)
  const [feedback, setFeedback] = useState<{ severity: 'success' | 'error' | 'warning'; text: string } | null>(null)
  const [pendingRemoval, setPendingRemoval] = useState<EuiccNotification | null>(null)
  const [collapsedGroups, setCollapsedGroups] = useState<Set<string>>(() => new Set())
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
  const toggleGroup = (iccid: string) => setCollapsedGroups((current) => {
    const next = new Set(current)
    if (next.has(iccid)) next.delete(iccid)
    else next.add(iccid)
    return next
  })

  return (
    <>
      <Dialog open onClose={() => !busy && onClose()} fullWidth maxWidth="sm"
        slotProps={{ paper: { sx: { width: 600, height: 'min(80vh, 760px)' } } }}>
        <DialogTitle>通知管理</DialogTitle>
        <DialogContent dividers sx={{ px: { xs: 2, sm: 3 } }}>
          <Stack spacing={1.5}>
            <Typography variant="body2" color="text.secondary">
              发送后保留通知记录。“全部删除”仅删除 enable / disable 通知。
            </Typography>
            {feedback && <Alert severity={feedback.severity} sx={{ overflowWrap: 'anywhere' }}>{feedback.text}</Alert>}
            {loadError && <Alert severity="error">{loadError}</Alert>}
            {loading && <Box sx={{ textAlign: 'center', py: 2 }}><CircularProgress size={24} aria-label="正在读取通知" /></Box>}
            {!loading && !loadError && !notifications.length && <Alert severity="info">没有待处理的通知。</Alert>}
            {Array.from(groups, ([iccid, items]) => {
              const expanded = !collapsedGroups.has(iccid)
              const contentId = `euicc-notifications-${iccid}`
              return (
                <Box component="section" key={iccid} data-notification-group={iccid} aria-label={`ICCID ${iccid}`}>
                  <ButtonBase onClick={() => toggleGroup(iccid)} aria-expanded={expanded} aria-controls={contentId}
                    aria-label={`${expanded ? '折叠' : '展开'} ICCID ${iccid} 的通知`} title={`ICCID：${iccid}`}
                    sx={{ display: 'flex', width: '100%', gap: 1, py: 0.75, mb: expanded ? 1 : 0, borderRadius: 1,
                      '&.Mui-focusVisible': { outline: '2px solid', outlineColor: 'primary.main' } }}>
                    <Box aria-hidden sx={{ flex: 1, minWidth: 8, borderTop: 1, borderColor: 'divider' }} />
                    <Typography component="span" variant="body2" noWrap
                      sx={{ fontWeight: 700, fontFamily: 'monospace', fontSize: { xs: '0.78rem', sm: '0.85rem' }, minWidth: 0 }}>
                      ICCID：{iccid}
                    </Typography>
                    <ExpandMore fontSize="small" sx={{ flexShrink: 0, transform: expanded ? 'none' : 'rotate(-90deg)' }} />
                    <Box aria-hidden sx={{ flex: 1, minWidth: 8, borderTop: 1, borderColor: 'divider' }} />
                  </ButtonBase>
                  <Collapse in={expanded} unmountOnExit id={contentId}>
                    <Box sx={{ display: 'grid', gridTemplateColumns: { xs: 'minmax(0, 1fr)', sm: 'repeat(2, minmax(0, 1fr))' }, gap: 1 }}>
                      {items.map((notification) => (
                        <Card key={notification.seqNumber} variant="outlined" data-notification={notification.seqNumber}
                          sx={{ display: 'flex', flexDirection: 'column' }}>
                          <CardContent sx={{ p: 1.5, pb: 0.75, flex: 1 }}>
                            <Typography variant="body2" fontWeight={600} sx={{ mb: 0.5 }}>通知 #{notification.seqNumber}</Typography>
                            <Typography variant="body2" sx={{ overflowWrap: 'anywhere' }}>
                              操作：{notification.profileManagementOperation}
                              {operationLabels[notification.profileManagementOperation] || ''}
                            </Typography>
                          </CardContent>
                          <CardActions sx={{ px: 1, pb: 1, pt: 0, justifyContent: 'flex-end' }}>
                            <Button size="small" aria-label={`发送通知 ${notification.seqNumber}`} disabled={disabled}
                              startIcon={busy === `process:${notification.seqNumber}` ? <CircularProgress size={16} /> : <Send />}
                              onClick={() => void act('process', { seq_number: notification.seqNumber })}>
                              发送
                            </Button>
                            <Button size="small" aria-label={`删除通知 ${notification.seqNumber}`} color="error" disabled={disabled}
                              startIcon={busy === `remove:${notification.seqNumber}` ? <CircularProgress size={16} /> : <DeleteOutline />}
                              onClick={() => safeToRemove(notification)
                                ? void act('remove', { seq_number: notification.seqNumber }) : setPendingRemoval(notification)}>
                              删除
                            </Button>
                          </CardActions>
                        </Card>
                      ))}
                    </Box>
                  </Collapse>
                </Box>
              )
            })}
            <Stack direction="row" spacing={1} justifyContent="flex-end" data-notification-bulk-actions
              sx={{ pt: 1.5, borderTop: 1, borderColor: 'divider' }}>
              <Button size="small" disabled={disabled || !notifications.length}
                startIcon={busy === 'process:all' ? <CircularProgress size={16} /> : <Send />}
                onClick={() => void act('process', { all: true })}>
                全部发送
              </Button>
              <Button size="small" color="error" disabled={disabled || !notifications.some(safeToRemove)}
                startIcon={busy === 'remove:all' ? <CircularProgress size={16} /> : <DeleteOutline />}
                onClick={() => void act('remove', { all: true })}>
                全部删除
              </Button>
            </Stack>
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
