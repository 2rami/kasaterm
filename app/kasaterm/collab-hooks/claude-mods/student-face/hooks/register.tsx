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

// 엔진이 고른 영어 낱말(Sauteing…) 대신 학생이 무엇을 하는지 한국어로.
const DOING: Record<string, string> = {
  requesting: '정리하는 중',
  thinking: '생각하는 중',
  responding: '답 쓰는 중',
  'tool-input': '도구 준비하는 중',
  'tool-use': '확인하는 중',
}

// 받침이 있으면 「이」, 없으면 「가」 — 학생 이름 뒤 주격 조사.
function subject(name: string): string {
  const last = name.charCodeAt(name.length - 1)
  const hangul = last >= 0xac00 && last <= 0xd7a3
  return `${name}${hangul && (last - 0xac00) % 28 !== 0 ? '이' : '가'}`
}

function took(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000))
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  const parts = [h ? `${h}시간` : '', m ? `${m}분` : '', h || m ? (s % 60 ? `${s % 60}초` : '') : `${s}초`].filter(Boolean)
  return `${parts.join(' ')} 걸려 끝났어요`
}

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

  // 말하는 자리에 얼굴만 — 턴의 첫 답 왼쪽에 아바타처럼. 이름은 칸 머리·상태가 이미 말해 빼었다(2026-10-06).
  on('ui.render', { component: 'AssistantMessage' }, async ($, e, next) => {
    const face = known.face
    if (!face?.file || e.surface !== 'terminal' || !e.props.isFirstOfReply) return next(e)
    if (turn.waiting) {
      turn.waiting = false
      turn.firsts.add(e.requestId)
      if (turn.firsts.size > FIRSTS_MAX) turn.firsts.delete(turn.firsts.values().next().value as string)
    }
    if (!turn.firsts.has(e.requestId)) return next(e)
    const body = await next(e)
    const { Box, Image } = $.ui.resolve(e)
    return (
      <Box flexDirection="row" gap={1}>
        <Image key="student-face" source={{ file: face.file, format: 'png', generation: face.generation }} columns={4} rows={2} alt=" " />
        <Box flexDirection="column" flexGrow={1} flexShrink={1}>{body}</Box>
      </Box>
    )
  })

  on('ui.render', { component: 'Spinner' }, ($, e, next) => {
    const face = known.face
    if (!face || e.props.message !== null) return next(e)
    return next({ ...e, props: { ...e.props, message: `${subject(face.name)} ${DOING[e.props.mode] ?? '일하는 중'}` } })
  })

  // 턴 끝 줄: 엔진의 영어 한 줄(Baked for 3s) 대신 학생 색 한국어, 앞 표지는 얼굴.
  on('ui.render', { component: 'TurnDuration' }, ($, e, next) => {
    const face = known.face
    if (!face) return next(e)
    const { Box, Image, Text } = $.ui.resolve(e)
    return (
      <Box flexDirection="row" gap={1}>
        {face.file ? (
          <Image key="student-face-end" source={{ file: face.file, format: 'png', generation: face.generation }} columns={2} rows={1} alt="◆" />
        ) : (
          <Text color={face.color}>◆</Text>
        )}
        <Text color={face.color}>{took(e.props.durationMs)}</Text>
      </Box>
    )
  })
}
