export const GROUP_COLORS = Object.freeze(['blue', 'red', 'yellow', 'green', 'pink', 'purple', 'cyan', 'orange', 'grey'])

export function chooseGroupColor(groups, preferred = 'blue') {
  const counts = new Map(GROUP_COLORS.map((color) => [color, 0]))
  for (const group of groups) {
    if (counts.has(group.color)) counts.set(group.color, counts.get(group.color) + 1)
  }
  const minimum = Math.min(...counts.values())
  if (counts.get(preferred) === minimum) return preferred
  return GROUP_COLORS.find((color) => counts.get(color) === minimum)
}

// 먼저 나온 고유색을 예약해야 앞쪽 중복을 풀면서 뒤쪽 정상 그룹의 색을 빼앗지 않는다.
export function planGroupColors(groups) {
  const reserved = []
  const duplicates = []
  const seen = new Set()
  for (const group of groups) {
    if (GROUP_COLORS.includes(group.color) && !seen.has(group.color)) {
      seen.add(group.color)
      reserved.push(group)
    } else duplicates.push(group)
  }
  const changes = []
  for (const group of duplicates) {
    const color = chooseGroupColor(reserved, group.color)
    reserved.push({ ...group, color })
    if (color !== group.color) changes.push({ groupId: group.id, color })
  }
  return changes
}
