import assert from 'node:assert/strict'
import { test } from 'node:test'
import { browseEmulation } from './browse-emulation.mjs'

test('no phone selected → no emulation', () => {
  assert.equal(browseEmulation({}), null)
  assert.equal(browseEmulation({ browse_device: '' }), null)
  assert.equal(browseEmulation({ browse_device: '~abc', browse_viewport: { width: 393, height: 852 } }), null)
})

test('phone with a reported screen uses that screen', () => {
  const emu = browseEmulation({ browse_device: 'phone:geono', browse_viewport: { width: 430, height: 932, dpr: 3 } })
  assert.deepEqual(emu, { width: 430, height: 932, deviceScaleFactor: 3, mobile: true, touch: true, fit: true })
})

test('phone without a reported screen falls back to an iPhone-sized viewport', () => {
  const emu = browseEmulation({ browse_device: 'phone:geono' })
  assert.equal(emu.width, 393)
  assert.equal(emu.mobile, true)
})
