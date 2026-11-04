/** Short relative time, for example `5 min ago`. */
export function relative(ms: number, now: number): string {
  const s = Math.max(0, Math.round((now - ms) / 1000))
  if (s < 60) return 'just now'
  if (s < 3600) return `${Math.round(s / 60)} min ago`
  if (s < 86_400) return `${Math.round(s / 3600)} h ago`
  const days = Math.round(s / 86_400)
  return days === 1 ? '1 day ago' : `${days} days ago`
}
