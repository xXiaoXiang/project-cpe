import assert from 'node:assert/strict'
import { test } from 'node:test'
import { existsSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { formatCarrierName, getCarrierInfo, getCarrierLogo } from '../src/utils/carriers.ts'

void test('Hong Kong PLMNs share operator identities and available logos', () => {
  const groups: [string[], string, string][] = [
    [['00', '02', '10', '18'], 'CSL', 'csl.svg'],
    [['03', '04', '14'], '3HK', 'three_hk.svg'],
    [['07'], '中国联通香港', 'china-unicom.svg'],
    [['12', '13'], '中国移动香港', 'china-mobile.svg'],
    [['16', '19', '29'], 'HKT', 'hkt.svg'],
    [['15'], 'SmarTone', 'smartone.svg'],
    [['31'], '中国电信香港', 'china-telecom.svg'],
  ]
  for (const [mncs, name, logo] of groups) {
    for (const mnc of mncs) {
      assert.equal(formatCarrierName('454', mnc), name)
      assert.equal(getCarrierLogo('454', mnc), `/provider/${logo}`)
    }
    assert.ok(existsSync(fileURLToPath(new URL(`../public/provider/${logo}`, import.meta.url))))
  }
})

void test('numeric MNC zero works without merging distinct three-digit MNCs', () => {
  assert.equal(formatCarrierName(454, 0), 'CSL')
  assert.equal(getCarrierLogo(454, 0), '/provider/csl.svg')
  assert.equal(formatCarrierName(460, 0), '中国移动')
  assert.equal(formatCarrierName('454', '003'), '454-003')
  assert.equal(getCarrierLogo('454', '003'), null)
  assert.equal(formatCarrierName(999, 0), '999-0')
  assert.equal(formatCarrierName('454', undefined), '未知')
  assert.equal(getCarrierInfo('454', ''), null)
})

void test('HK badges apply only to the three shared Hong Kong carrier logos', () => {
  for (const mnc of ['07', '12', '13', '31']) {
    assert.equal(getCarrierInfo('454', mnc)?.logoBadge, 'HK')
  }
  for (const mnc of ['00', '03', '15', '16']) {
    assert.equal(getCarrierInfo('454', mnc)?.logoBadge, undefined)
  }
  for (const mnc of ['00', '01', '03']) {
    assert.equal(getCarrierInfo('460', mnc)?.logoBadge, undefined)
    assert.ok(getCarrierLogo('460', mnc))
  }
})
