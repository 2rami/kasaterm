import test from 'node:test'
import assert from 'node:assert/strict'
import { withLocalProfile } from './profile-fill.mjs'

const LOCAL = { slug: 'sakiri', profile: 'data:image/png;base64,LOCAL' }
const lookup = (name) => (name === '사키리' ? LOCAL : { slug: null, profile: null })

test('a face sent from another machine is left alone', () => {
  // 보낸 쪽이 자기 그림을 실어 보냈으면 그게 정본이다. 덮으면 그 기계에서 고른 테마가 무시된다.
  const sent = { name: '사키리', profile: 'data:image/png;base64,THEIRS', slug: 'sakiri' }
  assert.equal(withLocalProfile(sent, lookup), sent)
})

test('a face missing from the other machine is filled from this one', () => {
  // 프사는 MCP 가 도는 기계에서 읽어 오므로, 다른 기기의 학생은 빈 채로 온다. 브리지는 크롬과
  // 같은 기계에서 도니 여기서 채워야 칩에 얼굴이 뜬다.
  const out = withLocalProfile({ name: '사키리', profile: null }, lookup)
  assert.equal(out.profile, LOCAL.profile)
  assert.equal(out.slug, 'sakiri')
})

test('a name this machine does not know keeps its monogram', () => {
  const sent = { name: '모르는이름', profile: null }
  assert.equal(withLocalProfile(sent, lookup), sent)
})

test('a broken roster never kills the identity', () => {
  // 신원이 null 이 되면 오버레이·활동 로그·툴바 아이콘이 통째로 죽는다 — 툴은 멀쩡히 도는데
  // UI 만 고장난 것처럼 보이는, 이 프로젝트가 한 번 겪은 실패다.
  const sent = { name: '사키리', profile: null }
  const boom = () => { throw new Error('roster unreadable') }
  assert.equal(withLocalProfile(sent, boom), sent)
  // 어댑터가 아예 없는 배포 빌드에서도 같다.
  assert.equal(withLocalProfile(sent, undefined), sent)
  assert.equal(withLocalProfile(null, lookup), null)
})
