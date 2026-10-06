import { describe, expect, test } from 'claude-code/testing'

import {
  activityLabel,
  askKey,
  gitTouch,
  launchedTask,
  mergeTasks,
  permissionPreview,
  running,
  stopTasks,
  taskNotification,
  usageEvent,
} from '../hooks/bridge'

describe('permission preview', () => {
  test('names the command, path or url the dialog asks about', () => {
    expect(permissionPreview('Bash', { command: 'rm -rf build\n  && ls' })).toBe('rm -rf build && ls')
    expect(permissionPreview('Edit', { file_path: '/repo/a.rs', old_string: 'x' })).toBe('/repo/a.rs')
    expect(permissionPreview('WebFetch', { url: 'https://example.com', prompt: 'p' })).toBe('https://example.com')
    expect(permissionPreview('mcp__x__y', {})).toBe('{}')
  })

  test('keeps one bounded line', () => {
    expect(permissionPreview('Bash', { command: 'a'.repeat(1000) }).length).toBe(400)
  })

  test('the ask key ties tool.check to the PermissionRequest of the same call', () => {
    expect(askKey('Bash', { command: 'ls' })).toBe(askKey('Bash', { command: 'ls' }))
    expect(askKey('Bash', { command: 'ls' })).not.toBe(askKey('Bash', { command: 'pwd' }))
  })
})

describe('activity', () => {
  test('a description wins over the raw input', () => {
    expect(activityLabel('Bash', { command: 'cargo test', description: 'Run tests' })).toBe('Run tests')
    expect(activityLabel('Read', { file_path: '/a/b.rs' })).toBe('/a/b.rs')
    expect(activityLabel('TodoWrite', undefined)).toBe('')
  })
})

describe('background tasks', () => {
  test('a backgrounded shell or monitor becomes a running task', () => {
    expect(launchedTask('Bash', { command: 'sleep 9', run_in_background: true }, { stdout: '', backgroundTaskId: 'b1' }))
      .toEqual({ id: 'b1', type: 'shell', status: 'running', label: 'sleep 9' })
    expect(launchedTask('Monitor', { description: 'watch log' }, { taskId: 'm1' })?.type).toBe('monitor')
    expect(launchedTask('Bash', { command: 'ls' }, { stdout: 'x' })).toBe(null)
  })

  test('the notification envelope names the finished task', () => {
    expect(taskNotification('<task-notification><task-id>b1</task-id><status>failed</status></task-notification>'))
      .toEqual({ id: 'b1', status: 'failed' })
    expect(taskNotification('plain text')).toBe(null)
  })

  test('subagents come from the engine list, the rest from shells and the stop snapshot', () => {
    const agents = [
      { id: 'a1', description: 'Explore code', type: 'Explore', status: 'running' },
      { id: 't1', description: 'mate', type: 'teammate', status: 'running' },
      { id: 'a2', description: 'done', type: 'Plan', status: 'completed' },
    ]
    const others = stopTasks([
      { id: 'b1', type: 'shell', status: 'running', command: 'npm run dev' },
      { id: 'a1', type: 'subagent', status: 'running', description: 'dup' },
    ])
    const merged = mergeTasks(agents, others.filter(t => t.type !== 'subagent'))
    expect(merged.map(t => t.id)).toEqual(['a1', 'a2', 'b1'])
    expect(running(merged).map(t => t.id)).toEqual(['a1', 'b1'])
    expect(merged[2]?.label).toBe('npm run dev')
  })
})

describe('usage', () => {
  test('the status line figures cross in the contract shape', () => {
    const event = usageEvent(
      { context: { tokens: 1000, window: 200000, percent: 1 }, rateLimits: [{ kind: 'five_hour', percentUsed: 23.5, resetsAt: 'T' }], cost: { usd: 0.42 } },
      5,
    )
    expect(event).toEqual({
      kind: 'usage',
      at: 5,
      context: { tokens: 1000, window: 200000, percent: 1 },
      limits: [{ kind: 'five_hour', percent: 23.5, resets_at: 'T' }],
      cost_usd: 0.42,
    })
  })
})

describe('git touch', () => {
  test('an edit names the file it wrote', () => {
    expect(gitTouch('Edit', { file_path: '/repo/a.rs', old_string: 'x', new_string: 'y' })).toEqual({ tool: 'Edit', verb: '', paths: ['/repo/a.rs'] })
    expect(gitTouch('NotebookEdit', { notebook_path: '/repo/n.ipynb' })?.paths).toEqual(['/repo/n.ipynb'])
  })

  test('reading commands leave the git column alone', () => {
    for (const command of ['ls -la', 'git status --short', 'cd /repo && git log --oneline -5', 'rg foo | head', 'cat a > /dev/null 2>&1', 'sed -n 1,20p a.rs', 'git --no-pager diff']) {
      expect(gitTouch('Bash', { command })).toBe(null)
    }
    expect(gitTouch('Read', { file_path: '/repo/a.rs' })).toBe(null)
  })

  test('a command that may change the tree names only its verb', () => {
    expect(gitTouch('Bash', { command: 'cd /repo && git commit -m "secret words"' })).toEqual({ tool: 'Bash', verb: 'git commit', paths: [] })
    expect(gitTouch('Bash', { command: 'git -C /repo checkout -b x' })?.verb).toBe('git checkout')
    expect(gitTouch('Bash', { command: 'TOKEN=abc cargo fmt' })?.verb).toBe('cargo')
    expect(gitTouch('Bash', { command: 'sed -i "" s/a/b/ f.rs' })?.verb).toBe('sed')
    expect(gitTouch('Bash', { command: "cat > notes.md <<'EOF'\nhello world\nEOF" })?.verb).toBe('cat')
  })
})
