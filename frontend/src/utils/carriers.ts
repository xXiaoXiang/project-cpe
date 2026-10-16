/*
 * @Author: 1orz cloudorzi@gmail.com
 * @Date: 2025-11-23 03:05:36
 * @LastEditors: 1orz cloudorzi@gmail.com
 * @LastEditTime: 2025-12-13 12:45:08
 * @FilePath: /udx710-backend/frontend/src/utils/carriers.ts
 * @Description:
 *
 * Copyright (c) 2025 by 1orz, All Rights Reserved.
 */

export interface CarrierInfo {
  mccMnc: string
  mcc: string
  mnc: string
  operatorCn: string
  operatorEn: string
  brand: string
  status: string
  technology?: string
  notes?: string
  logo?: string
  logoBadge?: string
  color?: 'default' | 'primary' | 'secondary' | 'error' | 'info' | 'success' | 'warning'
}

export const CHINA_CARRIERS: CarrierInfo[] = [
  {
    mccMnc: '46000',
    mcc: '460',
    mnc: '00',
    operatorCn: '中国移动',
    operatorEn: 'China Mobile',
    brand: '中国移动',
    status: '运营中',
    technology: 'GSM 900 / GSM 1800 / TD-SCDMA 1880 / TD-SCDMA 2010 / TD-LTE 1800/2300/2600',
  },
  {
    mccMnc: '46001',
    mcc: '460',
    mnc: '01',
    operatorCn: '中国联通',
    operatorEn: 'China Unicom',
    brand: '中国联通',
    status: '运营中',
    technology: 'GSM 900 / GSM 1800 / UMTS 2100 / TD-LTE 2300/2600 / FDD-LTE 1800/2100',
  },
  {
    mccMnc: '46002',
    mcc: '460',
    mnc: '02',
    operatorCn: '中国移动',
    operatorEn: 'China Mobile',
    brand: '中国移动',
    status: '运营中',
    technology: 'GSM 900 / GSM 1800 / TD-SCDMA 1880 / TD-SCDMA 2010',
  },
  {
    mccMnc: '46003',
    mcc: '460',
    mnc: '03',
    operatorCn: '中国电信',
    operatorEn: 'China Telecom',
    brand: '中国电信',
    status: '运营中',
    technology: 'CDMA2000 800 / CDMA2000 2100 / TD-LTE 2300/2600 / FDD-LTE 1800/2100 / EV-DO / eHRPD',
  },
  {
    mccMnc: '46005',
    mcc: '460',
    mnc: '05',
    operatorCn: '中国电信',
    operatorEn: 'China Telecom',
    brand: '中国电信',
    status: '运营中',
  },
  {
    mccMnc: '46006',
    mcc: '460',
    mnc: '06',
    operatorCn: '中国联通',
    operatorEn: 'China Unicom',
    brand: '中国联通',
    status: '运营中',
    technology: 'GSM 900 / GSM 1800 / UMTS 2100',
  },
  {
    mccMnc: '46007',
    mcc: '460',
    mnc: '07',
    operatorCn: '中国移动',
    operatorEn: 'China Mobile',
    brand: '中国移动',
    status: '运营中',
    technology: 'GSM 900 / GSM 1800 / TD-SCDMA 1880 / TD-SCDMA 2010',
  },
  {
    mccMnc: '46009',
    mcc: '460',
    mnc: '09',
    operatorCn: '中国联通',
    operatorEn: 'China Unicom',
    brand: '中国联通',
    status: '运营中',
  },
  {
    mccMnc: '46011',
    mcc: '460',
    mnc: '11',
    operatorCn: '中国电信',
    operatorEn: 'China Telecom',
    brand: '中国电信',
    status: '运营中',
    technology: 'CDMA2000 800 / CDMA2000 2100 / TD-LTE 2300/2600 / FDD-LTE 1800/2100 / EV-DO / eHRPD',
  },
  {
    mccMnc: '46015',
    mcc: '460',
    mnc: '15',
    operatorCn: '中国广电',
    operatorEn: 'China Broadnet',
    brand: '中国广电',
    status: '运营中',
    technology: 'LTE 1800 / LTE 900 / TD-LTE 1900 / TD-LTE 2300 / 5G 700 / 5G 2500',
  },
  {
    mccMnc: '46020',
    mcc: '460',
    mnc: '20',
    operatorCn: '中国铁通',
    operatorEn: 'China Tietong',
    brand: '中国铁通',
    status: '运营中',
    technology: 'GSM-R',
  },
]

// MCC 454: several PLMNs can belong to the same Hong Kong operator.
const HONG_KONG_OPERATOR_GROUPS = [
  { mncs: ['00', '02', '10', '18'], operatorCn: 'CSL', operatorEn: 'CSL Mobile Limited', brand: '1010 / csl. / SUN Mobile', logo: '/provider/csl.svg', color: 'warning' },
  { mncs: ['03', '04', '14'], operatorCn: '3HK', operatorEn: 'Hutchison Telecom', brand: '3HK', logo: '/provider/three_hk.svg', color: 'error' },
  { mncs: ['07'], operatorCn: '中国联通香港', operatorEn: 'China Unicom (Hong Kong)', brand: '中国联通香港', logo: '/provider/china-unicom.svg', logoBadge: 'HK', color: 'error' },
  { mncs: ['12', '13'], operatorCn: '中国移动香港', operatorEn: 'China Mobile Hong Kong', brand: 'CMHK', logo: '/provider/china-mobile.svg', logoBadge: 'HK', color: 'success' },
  { mncs: ['16', '19', '29'], operatorCn: 'HKT', operatorEn: 'Hong Kong Telecommunications', brand: 'HKT', logo: '/provider/hkt.svg', color: 'primary' },
  { mncs: ['15'], operatorCn: 'SmarTone', operatorEn: 'SmarTone', brand: '数码通', logo: '/provider/smartone.svg', color: 'error' },
  { mncs: ['31'], operatorCn: '中国电信香港', operatorEn: 'China Telecom (Hong Kong)', brand: '中国电信香港', logo: '/provider/china-telecom.svg', logoBadge: 'HK', color: 'primary' },
] satisfies (Pick<CarrierInfo, 'operatorCn' | 'operatorEn' | 'brand' | 'logo' | 'logoBadge' | 'color'> & { mncs: string[] })[]

export const HONG_KONG_CARRIERS: CarrierInfo[] = HONG_KONG_OPERATOR_GROUPS.flatMap(({ mncs, ...operator }) =>
  mncs.map((mnc) => ({ ...operator, mcc: '454', mnc, mccMnc: `454${mnc}`, status: '运营中' }))
)

const carrierMap = new Map<string, CarrierInfo>(
  [...CHINA_CARRIERS, ...HONG_KONG_CARRIERS].map((carrier) => [carrier.mccMnc, carrier])
)

function normalizePlmn(mcc: string | number | undefined, mnc: string | number | undefined) {
  if (mcc === undefined || mnc === undefined) return null
  const mccValue = String(mcc).trim()
  const mncValue = String(mnc).trim()
  if (!mccValue || !mncValue) return null
  // Keep three-digit MNCs distinct from two-digit MNCs (003 is not 03).
  return { mcc: mccValue.padStart(3, '0'), mnc: mncValue.padStart(2, '0') }
}

export function getCarrierInfo(mcc: string | number | undefined, mnc: string | number | undefined): CarrierInfo | null {
  const plmn = normalizePlmn(mcc, mnc)
  return plmn ? carrierMap.get(`${plmn.mcc}${plmn.mnc}`) ?? null : null
}

export function formatCarrierName(
  mcc: string | number | undefined,
  mnc: string | number | undefined,
  showEnglish = false
): string {
  const carrier = getCarrierInfo(mcc, mnc)

  if (!carrier) {
    if (normalizePlmn(mcc, mnc)) {
      return `${mcc}-${mnc}`
    }
    return '未知'
  }

  if (showEnglish) {
    return `${carrier.operatorCn} (${carrier.operatorEn})`
  }

  return carrier.operatorCn
}

export function getCarrierColor(
  mcc: string | number | undefined,
  mnc: string | number | undefined
): 'default' | 'primary' | 'secondary' | 'error' | 'info' | 'success' | 'warning' {
  const carrier = getCarrierInfo(mcc, mnc)
  if (carrier?.color) return carrier.color
  const plmn = normalizePlmn(mcc, mnc)
  if (!plmn) return 'default'

  const { mcc: mccStr, mnc: mncStr } = plmn

  if (mccStr === '460') {
    switch (mncStr) {
      case '00':
      case '02':
      case '07':
      case '08':
        return 'success'
      case '01':
      case '06':
      case '09':
        return 'error'
      case '03':
      case '05':
      case '11':
        return 'primary'
      case '15':
        return 'secondary'
    }
  }

  return 'default'
}

export function getCarrierLogo(mcc: string | number | undefined, mnc: string | number | undefined): string | null {
  const carrier = getCarrierInfo(mcc, mnc)
  if (carrier?.logo) return carrier.logo
  const plmn = normalizePlmn(mcc, mnc)
  if (!plmn) return null

  const { mcc: mccStr, mnc: mncStr } = plmn

  if (mccStr === '460') {
    switch (mncStr) {
      case '00':
      case '02':
      case '07':
      case '08':
        return '/provider/china-mobile.svg'
      case '01':
      case '06':
      case '09':
        return '/provider/china-unicom.svg'
      case '03':
      case '05':
      case '11':
        return '/provider/china-telecom.svg'
      case '15':
        return '/provider/china-broadnet.svg'
    }
  }

  return null
}
