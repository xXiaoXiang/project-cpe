import { useCallback, useEffect, useRef, useState } from 'react'
import {
  Alert,
  Box,
  Button,
  Card,
  CardActions,
  CardContent,
  Chip,
  CircularProgress,
  Dialog,
  DialogActions,
  DialogContent,
  DialogContentText,
  DialogTitle,
  IconButton,
  Snackbar,
  Stack,
  TextField,
  Tooltip,
  Typography,
} from '@mui/material'
import {
  DeleteOutline,
  Download,
  PowerSettingsNew,
  Refresh,
  SimCardDownload,
  Sync,
} from '@mui/icons-material'
import { api } from '@/api'
import type { EuiccDownloadRequest, EuiccInfo, EuiccProfile } from '@/api/types'

interface ProfileAction {
  type: 'enable' | 'delete'
  profile: EuiccProfile
}

interface Notice {
  severity: 'success' | 'error'
  text: string
}

const emptyDownloadRequest: EuiccDownloadRequest = {
  activation_code: '',
  smdp_address: '',
  matching_id: '',
  confirmation_code: '',
  imei: '',
}

export default function EuiccManagementPage() {
  const [euicc, setEuicc] = useState<EuiccInfo | null>(null)
  const [loading, setLoading] = useState(true)
  const [loadError, setLoadError] = useState<string | null>(null)
  const [pendingAction, setPendingAction] = useState<ProfileAction | null>(null)
  const [actionBusy, setActionBusy] = useState(false)
  const [refreshSimOpen, setRefreshSimOpen] = useState(false)
  const [refreshSimBusy, setRefreshSimBusy] = useState(false)
  const [downloadOpen, setDownloadOpen] = useState(false)
  const [downloadRequest, setDownloadRequest] = useState(emptyDownloadRequest)
  const [downloadError, setDownloadError] = useState<string | null>(null)
  const [downloadOpenOutput, setDownloadOpenOutput] = useState(false)
  const [downloadRunning, setDownloadRunning] = useState(false)
  const [downloadFinished, setDownloadFinished] = useState(false)
  const [downloadOutput, setDownloadOutput] = useState('')
  const [notice, setNotice] = useState<Notice | null>(null)
  const outputRef = useRef<HTMLPreElement | null>(null)

  const loadEuicc = useCallback(async () => {
    setLoading(true)
    setLoadError(null)
    try {
      const response = await api.getEuiccInfo()
      if (response.status !== 'ok' || !response.data) {
        throw new Error(response.message || '读取 eUICC 信息失败')
      }
      setEuicc(response.data)
    } catch (error) {
      setEuicc(null)
      setLoadError(error instanceof Error ? error.message : String(error))
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    void loadEuicc()
  }, [loadEuicc])

  useEffect(() => {
    if (outputRef.current) {
      outputRef.current.scrollTop = outputRef.current.scrollHeight
    }
  }, [downloadOutput])

  const runProfileAction = async () => {
    if (!pendingAction) return
    const action = pendingAction
    setActionBusy(true)
    try {
      if (action.type === 'enable') {
        const response = await api.enableEuiccProfile(action.profile.iccid)
        if (response.status !== 'ok' || !response.data) {
          throw new Error(response.message || '未确认到目标 Profile 已启用')
        }
        const { profiles } = response.data
        setEuicc((current) => (current ? { ...current, profiles } : current))
        setNotice({ severity: 'success', text: 'Profile 已切换' })
      } else {
        const response = await api.deleteEuiccProfile(action.profile.iccid)
        if (response.status !== 'ok') {
          throw new Error(response.message || 'Profile 删除失败')
        }
        setNotice({ severity: 'success', text: 'Profile 已删除' })
        await loadEuicc()
      }
      setPendingAction(null)
    } catch (error) {
      const reason = error instanceof Error ? error.message : String(error)
      setPendingAction(null)
      setNotice({
        severity: 'error',
        text: action.type === 'enable' ? `Profile 切换失败：${reason}` : reason,
      })
    } finally {
      setActionBusy(false)
    }
  }

  const runRefreshSim = async () => {
    setRefreshSimBusy(true)
    try {
      const response = await api.refreshEuiccSim()
      if (response.status !== 'ok') {
        throw new Error(response.message || '强制刷新失败')
      }
      setNotice({ severity: 'success', text: '已发送强制刷新 SIM 指令' })
      setRefreshSimOpen(false)
    } catch (error) {
      setNotice({ severity: 'error', text: error instanceof Error ? error.message : String(error) })
    } finally {
      setRefreshSimBusy(false)
    }
  }

  const updateDownloadRequest = (field: keyof EuiccDownloadRequest, value: string) => {
    setDownloadRequest((current) => ({ ...current, [field]: value }))
  }

  const startDownload = async () => {
    setDownloadError(null)
    setDownloadOutput('')
    setDownloadFinished(false)
    setDownloadRunning(true)
    setDownloadOpen(false)
    setDownloadOpenOutput(true)

    try {
      await api.downloadEuiccProfile(downloadRequest, (chunk) => {
        setDownloadOutput((current) => current + chunk)
      })
      setDownloadFinished(true)
      setNotice({ severity: 'success', text: 'LPAC 下载命令已完成' })
      await loadEuicc()
    } catch (error) {
      setDownloadError(error instanceof Error ? error.message : String(error))
    } finally {
      setDownloadRunning(false)
    }
  }

  const closeDownloadOutput = () => {
    if (!downloadRunning) setDownloadOpenOutput(false)
  }

  const hasActivationCode = Boolean(downloadRequest.activation_code?.trim())
  const euiccUnavailable = !loading && !euicc
  const canStartDownload = hasActivationCode || Boolean(
    downloadRequest.smdp_address?.trim() && downloadRequest.matching_id?.trim()
  )

  return (
    <Box sx={{ p: { xs: 2, md: 3 }, maxWidth: 1200, mx: 'auto' }}>
      <Stack
        direction={{ xs: 'column', sm: 'row' }}
        alignItems={{ xs: 'stretch', sm: 'center' }}
        justifyContent="space-between"
        spacing={2}
        sx={{ mb: 2.5 }}
      >
        <Box>
          <Typography variant="h4" component="h1" fontWeight={700}>
            eUICC 管理
          </Typography>
          {euicc && (
            <Typography variant="body2" color="text.secondary" sx={{ mt: 0.75, overflowWrap: 'anywhere' }}>
              EID <Box component="span" sx={{ fontFamily: 'monospace' }}>{euicc.eid}</Box>
              {euicc.firmware_version ? ` · 固件 ${euicc.firmware_version}` : ''}
            </Typography>
          )}
        </Box>
        <Stack direction="row" spacing={1} justifyContent={{ xs: 'flex-end', sm: 'initial' }}>
          <Tooltip title="重新读取 eUICC 和 Profile">
            <span>
              <Button
                variant="outlined"
                startIcon={loading ? <CircularProgress size={16} /> : <Refresh />}
                onClick={() => void loadEuicc()}
                disabled={loading || euiccUnavailable}
              >
                刷新
              </Button>
            </span>
          </Tooltip>
          <Button
            variant="outlined"
            color="warning"
            startIcon={<Sync />}
            onClick={() => setRefreshSimOpen(true)}
            disabled={!euicc || loading}
          >
            强制刷新 eUICC SIM
          </Button>
          <Button
            variant="contained"
            startIcon={<Download />}
            onClick={() => {
              setDownloadError(null)
              setDownloadOpen(true)
            }}
            disabled={!euicc || loading}
          >
            下载 Profile
          </Button>
        </Stack>
      </Stack>

      <Alert severity="warning" sx={{ mb: 2.5 }}>
        切换 Profile 可能使蜂窝网络短暂中断。删除 Profile 不可撤销。
      </Alert>

      {euiccUnavailable && (
        <Typography variant="body2" color="text.disabled" sx={{ mb: 2.5 }}>
          没有检测到可用的eUICC卡
        </Typography>
      )}

      {loadError && (
        <Alert severity="error" sx={{ mb: 2 }}>
          {loadError}
        </Alert>
      )}

      {loading && !euicc ? (
        <Box sx={{ display: 'flex', justifyContent: 'center', py: 8 }}>
          <CircularProgress />
        </Box>
      ) : euicc && euicc.profiles.length > 0 ? (
        <Box
          sx={{
            display: 'grid',
            gridTemplateColumns: 'repeat(auto-fit, minmax(min(100%, 320px), 1fr))',
            gap: 2,
          }}
        >
          {euicc.profiles.map((profile) => {
            const enabled = profile.profileState.toLowerCase() === 'enabled'
            const name = profile.profileNickname || profile.profileName || profile.serviceProviderName || '未命名 Profile'
            const provider = profile.serviceProviderName || profile.profileName
            return (
              <Card key={profile.iccid} variant="outlined" sx={{ borderRadius: 1, display: 'flex', flexDirection: 'column' }}>
                <CardContent sx={{ flexGrow: 1 }}>
                  <Stack direction="row" justifyContent="space-between" alignItems="flex-start" spacing={1}>
                    <Box sx={{ minWidth: 0 }}>
                      <Typography variant="h6" component="h2" sx={{ overflowWrap: 'anywhere' }}>
                        {name}
                      </Typography>
                      {provider && provider !== name && (
                        <Typography variant="body2" color="text.secondary" sx={{ mt: 0.25 }}>
                          {provider}
                        </Typography>
                      )}
                    </Box>
                    <Chip
                      size="small"
                      label={enabled ? '已启用' : '已停用'}
                      color={enabled ? 'success' : 'default'}
                      variant={enabled ? 'filled' : 'outlined'}
                    />
                  </Stack>
                  <Stack spacing={0.75} sx={{ mt: 2 }}>
                    <Typography variant="caption" color="text.secondary">ICCID</Typography>
                    <Typography variant="body2" sx={{ fontFamily: 'monospace', overflowWrap: 'anywhere' }}>
                      {profile.iccid}
                    </Typography>
                    {profile.profileClass && (
                      <Typography variant="body2" color="text.secondary" sx={{ mt: 0.5 }}>
                        类型：{profile.profileClass}
                      </Typography>
                    )}
                  </Stack>
                </CardContent>
                <CardActions sx={{ justifyContent: 'flex-end', px: 2, pb: 1.5 }}>
                  <Button
                    size="small"
                    startIcon={<PowerSettingsNew />}
                    onClick={() => setPendingAction({ type: 'enable', profile })}
                    disabled={enabled || actionBusy}
                  >
                    启用
                  </Button>
                  <Tooltip title={`删除 ${name}`}>
                    <span>
                      <IconButton
                        aria-label={`删除 ${name}`}
                        color="error"
                        onClick={() => setPendingAction({ type: 'delete', profile })}
                        disabled={actionBusy}
                      >
                        <DeleteOutline />
                      </IconButton>
                    </span>
                  </Tooltip>
                </CardActions>
              </Card>
            )
          })}
        </Box>
      ) : !loading && !loadError ? (
        <Alert severity="info">eUICC 已就绪，当前没有已安装的 Profile。</Alert>
      ) : null}

      <Dialog open={pendingAction !== null} onClose={() => !actionBusy && setPendingAction(null)}>
        <DialogTitle>{pendingAction?.type === 'delete' ? '确认删除 Profile' : '确认启用 Profile'}</DialogTitle>
        <DialogContent>
          <DialogContentText>
            {pendingAction && (
              <>
                {pendingAction.type === 'delete'
                  ? '此操作不可撤销，将删除：'
                  : '将启用：'}
                <Box component="span" sx={{ display: 'block', mt: 1, fontWeight: 600, color: 'text.primary', overflowWrap: 'anywhere' }}>
                  {pendingAction.profile.profileNickname || pendingAction.profile.profileName || pendingAction.profile.iccid}
                  {' · '}{pendingAction.profile.iccid}
                </Box>
                {pendingAction.type === 'enable' && '蜂窝连接可能暂时中断。'}
              </>
            )}
          </DialogContentText>
        </DialogContent>
        <DialogActions>
          <Button onClick={() => setPendingAction(null)} disabled={actionBusy}>取消</Button>
          <Button
            onClick={() => void runProfileAction()}
            color={pendingAction?.type === 'delete' ? 'error' : 'primary'}
            variant="contained"
            disabled={actionBusy}
            startIcon={actionBusy ? <CircularProgress size={16} color="inherit" /> : undefined}
          >
            {pendingAction?.type === 'delete' ? '删除' : '启用'}
          </Button>
        </DialogActions>
      </Dialog>

      <Dialog open={refreshSimOpen} onClose={() => !refreshSimBusy && setRefreshSimOpen(false)}>
        <DialogTitle>强制刷新 eUICC SIM</DialogTitle>
        <DialogContent>
          <DialogContentText sx={{ whiteSpace: 'pre-line' }}>
            {'切换Profile后请耐心等待30秒，强制刷新有可能造成eUICC失去响应。\n确认是否强制刷新？'}
          </DialogContentText>
        </DialogContent>
        <DialogActions>
          <Button onClick={() => setRefreshSimOpen(false)} disabled={refreshSimBusy}>取消</Button>
          <Button
            onClick={() => void runRefreshSim()}
            color="warning"
            variant="contained"
            disabled={refreshSimBusy}
            startIcon={refreshSimBusy ? <CircularProgress size={16} color="inherit" /> : undefined}
          >
            强制刷新
          </Button>
        </DialogActions>
      </Dialog>

      <Dialog open={downloadOpen} onClose={() => setDownloadOpen(false)} fullWidth maxWidth="sm">
        <DialogTitle>下载 eUICC Profile</DialogTitle>
        <DialogContent>
          <Stack spacing={2} sx={{ pt: 1 }}>
            {downloadError && <Alert severity="error">{downloadError}</Alert>}
            <TextField
              label="LPA 激活码"
              placeholder="LPA:1$SM-DP+ 域名$Matching ID"
              value={downloadRequest.activation_code ?? ''}
              onChange={(event) => updateDownloadRequest('activation_code', event.target.value)}
              fullWidth
              helperText="填写激活码后将优先使用此项；否则填写 SM-DP+ 地址和 Matching ID。"
            />
            <Typography variant="overline" color="text.secondary" sx={{ mt: 0.5 }}>
              或单独填写服务器参数
            </Typography>
            <TextField
              label="SM-DP+ 地址"
              value={downloadRequest.smdp_address ?? ''}
              onChange={(event) => updateDownloadRequest('smdp_address', event.target.value)}
              fullWidth
              disabled={hasActivationCode}
            />
            <TextField
              label="Matching ID"
              value={downloadRequest.matching_id ?? ''}
              onChange={(event) => updateDownloadRequest('matching_id', event.target.value)}
              fullWidth
              disabled={hasActivationCode}
            />
            <TextField
              label="确认码"
              value={downloadRequest.confirmation_code ?? ''}
              onChange={(event) => updateDownloadRequest('confirmation_code', event.target.value)}
              fullWidth
            />
            <TextField
              label="IMEI"
              value={downloadRequest.imei ?? ''}
              onChange={(event) => updateDownloadRequest('imei', event.target.value)}
              fullWidth
            />
          </Stack>
        </DialogContent>
        <DialogActions>
          <Button onClick={() => setDownloadOpen(false)}>取消</Button>
          <Button
            variant="contained"
            startIcon={<SimCardDownload />}
            onClick={() => void startDownload()}
            disabled={!canStartDownload}
          >
            开始下载
          </Button>
        </DialogActions>
      </Dialog>

      <Dialog open={downloadOpenOutput} onClose={closeDownloadOutput} fullWidth maxWidth="md">
        <DialogTitle>LPAC 下载输出</DialogTitle>
        <DialogContent>
          <Stack spacing={1.5}>
            {downloadRunning && <Alert severity="info">LPAC 正在运行，输出会实时追加。</Alert>}
            {downloadError && <Alert severity="error">{downloadError}</Alert>}
            {downloadFinished && !downloadError && <Alert severity="success">命令已结束。</Alert>}
            <Box
              component="pre"
              ref={outputRef}
              sx={{
                m: 0,
                p: 1.5,
                minHeight: 180,
                maxHeight: { xs: 300, sm: 420 },
                overflow: 'auto',
                border: 1,
                borderColor: 'divider',
                borderRadius: 1,
                bgcolor: 'action.hover',
                color: 'text.primary',
                fontFamily: 'monospace',
                fontSize: '0.8rem',
                whiteSpace: 'pre-wrap',
                overflowWrap: 'anywhere',
              }}
            >
              {downloadOutput || (downloadRunning ? '等待 LPAC 输出…' : '')}
            </Box>
          </Stack>
        </DialogContent>
        <DialogActions>
          <Button onClick={closeDownloadOutput} disabled={downloadRunning}>关闭</Button>
        </DialogActions>
      </Dialog>

      <Snackbar open={notice !== null} autoHideDuration={6000} onClose={() => setNotice(null)}>
        <Alert onClose={() => setNotice(null)} severity={notice?.severity || 'success'} variant="filled">
          {notice?.text}
        </Alert>
      </Snackbar>
    </Box>
  )
}
