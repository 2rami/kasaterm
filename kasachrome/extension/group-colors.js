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
