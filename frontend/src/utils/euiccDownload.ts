import type { LpacEvent, LpacPayload } from '../api/types.ts'

export const downloadSteps = [
  '准备中',
  '正在连接服务器',
  '正在向服务器认证您的设备',
  '正在下载 eSIM 配置文件',
  '正在写入 eSIM 配置文件',
]

const stages: Record<string, number> = {
  es10b_get_euicc_challenge_and_info: 0,
  es9p_initiate_authentication: 1,
  es10b_authenticate_server: 2,
  es9p_authenticate_client: 2,
  es8p_metadata_parse: 2,
  es10b_prepare_download: 2,
  es9p_get_bound_profile_package: 3,
  es10b_load_bound_profile_package: 4,
}

export interface DownloadProgress {
  step: number
  metadata?: { profileName?: string; serviceProviderName?: string; iccid?: string }
  result?: LpacPayload
}

export function advanceDownload(progress: DownloadProgress, event: LpacEvent): DownloadProgress {
  const { payload } = event
  // Session cleanup must not turn a connection/authentication error into a later stage.
  if (payload.message.includes('cancel_session')) return progress
  const stage = stages[payload.message]
  const next = { ...progress, step: stage === undefined ? progress.step : Math.max(progress.step, stage) }
  if (payload.message === 'es8p_metadata_parse' && payload.data && typeof payload.data === 'object') {
    const data = payload.data as Record<string, unknown>
    next.metadata = {
      profileName: typeof data.profileName === 'string' ? data.profileName : undefined,
      serviceProviderName: typeof data.serviceProviderName === 'string' ? data.serviceProviderName : undefined,
      iccid: typeof data.iccid === 'string' ? data.iccid : undefined,
    }
  }
  if (event.type === 'lpa') {
    next.result = payload
    if (payload.code === 0) next.step = downloadSteps.length - 1
    else if (stage !== undefined) next.step = stage
  }
  return next
}

export function downloadFailure(result: LpacPayload | undefined, fallback: string): string {
  if (!result || result.code === 0) {
    return /failed to fetch|networkerror|load failed/i.test(fallback)
      ? '无法连接到设备，请检查连接，并刷新列表确认是否已安装。' : fallback
  }
  const detail = typeof result.data === 'string' ? result.data
    : result.data === null || result.data === undefined ? '' : JSON.stringify(result.data)
  if (/HTTP transport failed|curl|resolve host|connect|timed? ?out/i.test(detail)) {
    return '无法与服务器正常通信，请检查网络和 SM-DP+ 地址后重试。'
  }
  if (/confirmation|confirm code/i.test(detail)) return '确认码校验失败，请检查确认码后重试。'
  return detail ? `下载失败：${detail}` : `下载失败（${result.message}，错误码 ${result.code}）`
}

export function downloadedIccid(progress: DownloadProgress): string | undefined {
  const data = progress.result?.data
  return data && typeof data === 'object' && 'iccid' in data && typeof data.iccid === 'string'
    ? data.iccid : progress.metadata?.iccid
}
