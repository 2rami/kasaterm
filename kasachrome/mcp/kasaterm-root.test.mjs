import assert from 'node:assert/strict'
import { mkdirSync, realpathSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { delimiter, join } from 'node:path'
import { test } from 'node:test'
import { findKasatermRoot } from './kasaterm-root.mjs'

function fixture(name) {
  const base = join(tmpdir(), `kasaterm-root-${process.pid}-${name}`)
  mkdirSync(join(base, 'app', 'kasaterm', 'collab-hooks'), { recursive: true })
  writeFileSync(join(base, 'app', 'kasaterm', 'collab-hooks', 'characters.json'), '{}')
  mkdirSync(join(base, 'bin'), { recursive: true })
  return realpathSync(base)
}

test('finds the renamed kasaterm checkout from PATH', () => {
  const base = fixture('renamed-kasaterm')
  try {
    assert.equal(findKasatermRoot({ PATH: join(base, 'bin') }), base)
  } finally {
    rmSync(base, { recursive: true, force: true })
  }
})

test('uses repository markers instead of depending on the checkout folder name', () => {
  const base = fixture('custom-checkout')
  try {
    const unrelated = join(base, 'elsewhere')
    mkdirSync(join(unrelated, 'bin'), { recursive: true })
    assert.equal(
      findKasatermRoot({ PATH: [join(unrelated, 'bin'), join(base, 'bin')].join(delimiter) }),
      base,
    )
  } finally {
    rmSync(base, { recursive: true, force: true })
  }
})

test('explicit configuration remains the first choice', () => {
  assert.equal(
    findKasatermRoot({ KASACHROME_KASATERM_DIR: '/opt/kasaterm', PATH: '' }),
    '/opt/kasaterm',
  )
})
