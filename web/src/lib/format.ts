/** Formats a duration in seconds as `m:ss`, or `h:mm:ss` past an hour. */
export function formatDuration(totalSeconds: number): string {
  const seconds = Math.max(0, Math.floor(totalSeconds))
  const hours = Math.floor(seconds / 3600)
  const minutes = Math.floor((seconds % 3600) / 60)
  const rest = seconds % 60

  const pad = (n: number) => n.toString().padStart(2, '0')
  return hours > 0
    ? `${hours}:${pad(minutes)}:${pad(rest)}`
    : `${minutes}:${pad(rest)}`
}

/** Formats a byte count with a binary unit, e.g. `433.3 MB`. */
export function formatBytes(bytes: number): string {
  if (bytes <= 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const exponent = Math.min(
    units.length - 1,
    Math.floor(Math.log(bytes) / Math.log(1024)),
  )
  const value = bytes / 1024 ** exponent
  // Whole bytes never need a decimal point.
  return `${exponent === 0 ? value : value.toFixed(1)} ${units[exponent]}`
}

/** Shortens a long file path to its last couple of segments. */
export function shortenPath(path: string, keep = 2): string {
  const parts = path.split(/[\\/]/).filter(Boolean)
  if (parts.length <= keep) return path
  return `…${path.includes('\\') ? '\\' : '/'}${parts.slice(-keep).join(path.includes('\\') ? '\\' : '/')}`
}
