import type { EngineInterface, Register } from 'claude-code'

type Face = { name: string; color?: string; file?: string; generation?: number }

// 학생은 칸이 뜰 때 정해지지만 외형 끄기·테마 바꾸기는 그 뒤에도 바뀐다 — 턴을 열 때 다시 묻되 이 간격보다 자주는 안 묻는다.
const REFRESH_MS = 30000

// 모듈 변수는 다시 실릴 때 처음으로 돌아간다 — session.start 가 다시 묻는다.
const known = { face: null as Face | null, at: 0 }

// 얼굴은 턴마다 한 번 — 그 턴의 첫 답 블록에만. 엔진의 isFirstOfReply 는 도구 줄 뒤 글마다 참이라
// 그대로 쓰면 한 턴에 얼굴이 몇 번씩 끼었다(2026-10-06 「위치가 이상해」). 블록은 메시지 id(requestId)로
// 기억해 스크롤로 다시 그려져도 같은 자리에 남는다.
const turn = { waiting: false, firsts: new Set<string>() }
const FIRSTS_MAX = 500

function same(a: Face | null, b: Face | null): boolean {
  return JSON.stringify(a) === JSON.stringify(b)
}

// 앱이 정본이다 — 칸의 학생 이름으로 색·얼굴 파일·외형 끄기를 묻는다. 앱에 못 닿으면 아는 것을 그대로 둔다.
async function ask($: EngineInterface) {
  known.at = Date.now()
  const name = (await $.env.get('KASATERM_CHARACTER')) ?? ''
  const port = (await $.env.get('KASASPACE_MCP_PORT')) ?? ''
  let face: Face | null = null
  if (name && /^\d+$/.test(port)) {
    try {
      const res = await $.http.fetch(`http://127.0.0.1:${port}/claude-mod/face?name=${encodeURIComponent(name)}`)
      if (!res.ok) return
      const body = JSON.parse(res.text) as Partial<Face>
      face = body.name ? { name: body.name, color: body.color ?? undefined, file: body.file ?? undefined, generation: body.generation ?? undefined } : null
    } catch {
      return
    }
  } else if (name) {
    face = { name }
  }
  if (!same(face, known.face)) {
    known.face = face
    $.ui.invalidate('ui.render')
  }
}

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    const result = await next(e)
    await ask($)
    return result
  })

  on('turn.start', async ($, e, next) => {
    turn.waiting = true
    if (Date.now() - known.at > REFRESH_MS) void ask($)
    return next(e)
  })

  on('ui.render', { component: 'AssistantMessage' }, async ($, e, next) => {
    const face = known.face
    if (!face || !e.props.isFirstOfReply) return next(e)
    if (turn.waiting) {
      turn.waiting = false
      turn.firsts.add(e.requestId)
      if (turn.firsts.size > FIRSTS_MAX) turn.firsts.delete(turn.firsts.values().next().value as string)
    }
    if (!turn.firsts.has(e.requestId)) return next(e)
    const body = await next(e)
    // 그림은 터미널만 그린다(다른 화면의 요소 표에는 Image 가 없다) — 거기엔 이름만.
    if (e.surface === 'terminal') {
      const { Box, Image, Text } = $.ui.resolve(e)
      return (
        <Box flexDirection="column">
          {/* 이름을 얼굴 아랫줄에 붙여, 얼굴 두 줄과 답 사이에 빈 줄이 안 생기게 한다. */}
          <Box flexDirection="row" gap={1} alignItems="flex-end">
            {face.file ? (
              <Image key="student-face" source={{ file: face.file, format: 'png', generation: face.generation }} columns={4} rows={2} alt=" " />
            ) : null}
            <Text bold color={face.color}>{face.name}</Text>
          </Box>
          {body}
        </Box>
      )
    }
    const { Box, Text } = $.ui.resolve(e)
    return (
      <Box flexDirection="column">
        <Text bold color={face.color}>{face.name}</Text>
        {body}
      </Box>
    )
  })
}
