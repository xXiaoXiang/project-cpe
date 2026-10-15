import { useCallback, useEffect, useRef, useState } from 'react'
import { useSingleFlight } from '../hooks/useSingleFlight'
import {
  Alert,
  Box,
  Button,
  Card,
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
  Check,
  CheckCircleOutline,
  Close,
  Download,
  EditOutlined,
  ErrorOutline,
  NotificationsNone,
  PowerSettingsNew,
  Refresh,
  SimCardDownload,
  Sync,
} from '@mui/icons-material'
import { api } from '@/api'
import type { EuiccDownloadRequest, EuiccInfo, EuiccProfile } from '@/api/types'
import EuiccNotificationsDialog from '../components/EuiccNotificationsDialog'
import { advanceDownload, downloadedIccid, downloadFailure, downloadSteps, type DownloadProgress } from '../utils/euiccDownload'

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

const profileName = (profile: EuiccProfile) => profile.profileName || profile.serviceProviderName || profile.iccid

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
  const [showRawOutput, setShowRawOutput] = useState(false)
  const [downloadProgress, setDownloadProgress] = useState<DownloadProgress>({ step: 0 })
  const [nicknameEdit, setNicknameEdit] = useState<{ iccid: string; value: string } | null>(null)
  const [nicknameBusy, setNicknameBusy] = useState(false)
  const [nicknameError, setNicknameError] = useState<string | null>(null)
  const [notificationsOpen, setNotificationsOpen] = useState(false)
  const [notice, setNotice] = useState<Notice | null>(null)
  const outputRef = useRef<HTMLPreElement | null>(null)

  const run = useSingleFlight()
  const loadEuicc = useCallback(() => run('euicc', async () => {
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
  }), [run])

  useEffect(() => {
    void loadEuicc()
  }, [loadEuicc])

  useEffect(() => {
    if (outputRef.current) {
      outputRef.current.scrollTop = outputRef.current.scrollHeight
    }
  }, [downloadOutput, showRawOutput])

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

  const saveNickname = async () => {
    if (!nicknameEdit || nicknameBusy) return
    if (Array.from(nicknameEdit.value).length > 32) {
      setNicknameError('昵称最多 32 个字符')
      return
    }
    const { iccid, value } = nicknameEdit
    setNicknameBusy(true)
    setNicknameError(null)
    try {
      const response = await api.setEuiccNickname(iccid, value)
      if (response.status !== 'ok' || !response.data) throw new Error(response.message || '保存昵称失败')
      const nickname = response.data.nickname
      setEuicc((current) => current ? { ...current, profiles: current.profiles.map((profile) =>
        profile.iccid === iccid ? { ...profile, profileNickname: nickname || null } : profile) } : current)
      setNicknameEdit(null)
      setNotice({ severity: 'success', text: '昵称已保存' })
    } catch (error) {
      setNicknameError(error instanceof Error ? error.message : String(error))
    } finally {
      setNicknameBusy(false)
    }
  }

  const startDownload = async () => {
    if (downloadRunning) return
    setDownloadError(null)
    setDownloadOutput('')
    setShowRawOutput(false)
    setDownloadProgress({ step: 0 })
    setDownloadFinished(false)
    setDownloadRunning(true)
    setDownloadOpen(false)
    setDownloadOpenOutput(true)

    try {
      const result = await api.downloadEuiccProfile(downloadRequest, (chunk) => {
        setDownloadOutput((current) => current + chunk)
      }, (event) => {
        setDownloadProgress((current) => advanceDownload(current, event))
      })
      setDownloadProgress((current) => advanceDownload(current, { type: 'lpa', payload: result }))
      setDownloadFinished(true)
      setNotice({ severity: 'success', text: 'eSIM 配置文件下载成功' })
    } catch (error) {
      setDownloadError(error instanceof Error ? error.message : String(error))
    } finally {
      setDownloadRunning(false)
      // A lost stream may still have installed the profile; read the actual card state.
      void loadEuicc()
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
  const deviceBusy = actionBusy || nicknameBusy || downloadRunning || refreshSimBusy
  const nicknameLength = Array.from(nicknameEdit?.value || '').length
  const downloadSucceeded = downloadFinished && !downloadError
  const downloadIccid = downloadedIccid(downloadProgress)
  const downloadErrorText = downloadError ? downloadFailure(downloadProgress.result, downloadError) : null
  const activeProfile = euicc?.profiles.find((profile) =>
    profile.profileState.toLowerCase() === 'enabled' && profile.iccid !== pendingAction?.profile.iccid)

  return (
    <Box sx={{ p: { xs: 2, md: 3 }, maxWidth: 1200, mx: 'auto' }}>
      <Stack
        direction={{ xs: 'column', lg: 'row' }}
        alignItems={{ xs: 'stretch', lg: 'center' }}
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
        <Stack direction="row" useFlexGap flexWrap="wrap" spacing={1} justifyContent={{ xs: 'flex-start', lg: 'flex-end' }}>
          <Tooltip title="重新读取 eUICC 和 Profile">
            <span>
              <Button
                variant="outlined"
                startIcon={loading ? <CircularProgress size={16} /> : <Refresh />}
                onClick={() => void loadEuicc()}
                disabled={loading || euiccUnavailable || deviceBusy}
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
            disabled={!euicc || loading || deviceBusy}
          >
            强制刷新 eUICC SIM
          </Button>
          <Button variant="outlined" startIcon={<NotificationsNone />}
            onClick={() => setNotificationsOpen(true)} disabled={!euicc || loading || deviceBusy}>
            通知管理
          </Button>
          <Button
            variant="contained"
            startIcon={<Download />}
            onClick={() => {
              setDownloadError(null)
              setDownloadOpen(true)
            }}
            disabled={!euicc || loading || deviceBusy}
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
            alignItems: 'start',
            gap: 2,
          }}
        >
          {euicc.profiles.map((profile) => {
            const enabled = profile.profileState.toLowerCase() === 'enabled'
            const name = profile.profileName || profile.serviceProviderName || '未命名 Profile'
            const provider = profile.serviceProviderName || profile.profileName
            const editing = nicknameEdit?.iccid === profile.iccid
            return (
              <Card key={profile.iccid} variant="outlined" sx={{ borderRadius: 1, display: 'flex', flexDirection: 'column' }}>
                <CardContent sx={{ p: 2, '&:last-child': { pb: 2 } }}>
                  <Stack direction="row" justifyContent="space-between" alignItems="flex-start" spacing={1}>
                    <Box sx={{ minWidth: 0 }}>
                      <Stack direction="row" alignItems="baseline" gap={0.75} flexWrap="wrap">
                        <Typography variant="h6" component="h2" sx={{ overflowWrap: 'anywhere' }}>{name}</Typography>
                        {!editing && profile.profileNickname && (
                          <Typography variant="body2" color="text.secondary" sx={{ overflowWrap: 'anywhere' }}>{profile.profileNickname}</Typography>
                        )}
                        {!editing && <Tooltip title="编辑昵称"><span>
                          <IconButton size="small" aria-label={`编辑 ${name} 的昵称`} disabled={deviceBusy}
                            onClick={() => {
                              setNicknameEdit({ iccid: profile.iccid, value: profile.profileNickname || '' })
                              setNicknameError(null)
                            }}><EditOutlined fontSize="small" /></IconButton>
                        </span></Tooltip>}
                      </Stack>
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
                  {editing && nicknameEdit && (
                    <Box component="form" onSubmit={(event) => { event.preventDefault(); void saveNickname() }} sx={{ mt: 1.5 }}>
                      <Stack direction="row" alignItems="flex-start" spacing={0.5}>
                        <TextField autoFocus size="small" label="昵称" fullWidth value={nicknameEdit.value}
                          disabled={nicknameBusy} error={nicknameLength > 32 || Boolean(nicknameError)}
                          helperText={nicknameError || `${nicknameLength}/32 字符，留空可清除昵称`}
                          onChange={(event) => {
                            setNicknameEdit({ iccid: profile.iccid, value: event.target.value })
                            setNicknameError(null)
                          }} />
                        <IconButton type="submit" color="primary" aria-label="保存昵称" disabled={nicknameBusy || nicknameLength > 32}>
                          {nicknameBusy ? <CircularProgress size={20} /> : <Check />}
                        </IconButton>
                        <IconButton aria-label="取消编辑昵称" disabled={nicknameBusy} onClick={() => setNicknameEdit(null)}><Close /></IconButton>
                      </Stack>
                    </Box>
                  )}
                  <Stack direction="row" alignItems="center" spacing={1.5} sx={{ mt: 1.5 }}>
                    <Stack spacing={0.75} sx={{ flex: 1, minWidth: 0 }}>
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
                    <Stack spacing={0.5} sx={{ flexShrink: 0 }}>
                      <Button size="small" startIcon={<PowerSettingsNew />} aria-label={`启用 ${name}`}
                        onClick={() => setPendingAction({ type: 'enable', profile })} disabled={enabled || deviceBusy}>
                        启用
                      </Button>
                      <Button size="small" startIcon={<DeleteOutline />} aria-label={`删除 ${name}`} color="error"
                        onClick={() => setPendingAction({ type: 'delete', profile })} disabled={deviceBusy}>
                        删除
                      </Button>
                    </Stack>
                  </Stack>
                </CardContent>
              </Card>
            )
          })}
        </Box>
      ) : !loading && !loadError ? (
        <Alert severity="info">eUICC 已就绪，当前没有已安装的 Profile。</Alert>
      ) : null}

      {notificationsOpen && <EuiccNotificationsDialog onClose={() => setNotificationsOpen(false)} />}

      <Dialog open={pendingAction !== null} onClose={() => !actionBusy && setPendingAction(null)}>
        <DialogTitle>{pendingAction?.type === 'delete' ? '确认删除 Profile' : '确认启用 Profile'}</DialogTitle>
        <DialogContent>
          <DialogContentText>
            {pendingAction?.type === 'enable' ? (
              activeProfile
                ? `启用${profileName(pendingAction.profile)}会关闭${profileName(activeProfile)}，确认是否继续？`
                : `启用${profileName(pendingAction.profile)}，确认是否继续？`
            ) : pendingAction && (
              <>
                此操作不可撤销，将删除：
                <Box component="span" sx={{ display: 'block', mt: 1, fontWeight: 600, color: 'text.primary', overflowWrap: 'anywhere' }}>
                  {pendingAction.profile.profileNickname || pendingAction.profile.profileName || pendingAction.profile.iccid}
                  {' · '}{pendingAction.profile.iccid}
                </Box>
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

      <Dialog open={downloadOpenOutput} onClose={closeDownloadOutput} fullWidth maxWidth="sm">
        <DialogTitle>下载 eSIM 配置文件</DialogTitle>
        <DialogContent>
          <Stack spacing={1.5}>
            <Typography role="status" aria-live="polite" color="text.secondary">
              {downloadRunning ? '正在下载您的 eSIM…' : downloadSucceeded ? 'eSIM 配置文件下载成功' : '下载未完成'}
            </Typography>
            <Box role="list" aria-label="eSIM 下载进度">
              {downloadSteps.map((label, index) => {
                const complete = downloadSucceeded || index < downloadProgress.step
                const active = !downloadSucceeded && index === downloadProgress.step
                const failed = active && Boolean(downloadError)
                const status = complete ? '已完成' : failed ? '失败' : active ? '进行中' : '等待中'
                return (
                  <Stack key={label} role="listitem" aria-label={`${label}：${status}`} aria-current={active ? 'step' : undefined}
                    direction="row" alignItems="center" justifyContent="space-between" spacing={2}
                    sx={{ py: 2.25, borderBottom: 1, borderColor: 'divider', color: failed ? 'error.main' : complete || active ? 'text.primary' : 'text.disabled' }}>
                    <Typography>{label}</Typography>
                    {complete ? <CheckCircleOutline color="success" /> : failed ? <ErrorOutline color="error" />
                      : active && downloadRunning ? <CircularProgress size={24} />
                        : <Box sx={{ width: 20, height: 20, borderRadius: '50%', border: 1, borderColor: 'divider', flexShrink: 0 }} />}
                  </Stack>
                )
              })}
            </Box>
            {downloadProgress.metadata?.profileName && (
              <Typography variant="body2" color="text.secondary">配置文件：{downloadProgress.metadata.profileName}</Typography>
            )}
            {downloadSucceeded && <Alert severity="success">
              配置文件已写入 eUICC，可返回列表管理。
              {downloadIccid && <Typography variant="body2" sx={{ mt: 0.5, overflowWrap: 'anywhere' }}>ICCID：{downloadIccid}</Typography>}
            </Alert>}
            {downloadErrorText && <Alert severity="error">{downloadErrorText}</Alert>}
            <Button variant="text" size="small" sx={{ alignSelf: 'flex-start' }} aria-expanded={showRawOutput}
              aria-controls="lpac-raw-output" onClick={() => setShowRawOutput((current) => !current)}>
              {showRawOutput ? '隐藏原始输出' : '显示原始输出'}
            </Button>
            {showRawOutput && <Box
              id="lpac-raw-output"
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
            </Box>}
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
