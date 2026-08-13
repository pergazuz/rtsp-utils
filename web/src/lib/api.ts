/**
 * Client for the rtsp-utils control API.
 *
 * Requests go to a relative `/api` path so the same build works whether it is
 * served by the Rust binary or proxied from the Vite dev server.
 */

export type StreamState = 'running' | 'stopped'

/** How the browser can preview a stream: MSE video, or a JPEG still. */
export type PreviewKind = 'video' | 'image'

export interface Track {
  kind: 'video' | 'audio'
  codec: string
  detail: string
  control: string
  /** The `codecs=` identifier for MSE; null for tracks the preview skips. */
  codecString: string | null
}

export interface Stream {
  name: string
  path: string
  url: string
  state: StreamState
  looping: boolean
  durationSeconds: number
  viewers: number
  /** Epoch milliseconds, or null while the stream is stopped. */
  startedAt: number | null
  /** Whether the browser can show a live preview of this stream. */
  previewable: boolean
  /** How to preview it, or null when the browser cannot. */
  preview: PreviewKind | null
  tracks: Track[]
}

export interface Health {
  ok: boolean
  version: string
  mediaRoot: string
  rtsp: {
    host: string
    port: number
    bind: string
    looping: boolean
  }
  streamCount: number
}

export interface FileEntry {
  name: string
  /** Absolute path on the server. */
  path: string
  directory: boolean
  size: number
}

/** A clickable location: one breadcrumb step, or one drive. */
export interface PathSegment {
  label: string
  path: string
}

export interface Listing {
  /** Absolute path of the directory being listed. */
  path: string
  /** Absolute path of the parent, or null at a filesystem root. */
  parent: string | null
  /** Where the picker opens. */
  start: string
  /** Whether browsing is restricted to `start`. */
  confined: boolean
  /** Set when the folder held more entries than the server will return. */
  truncated: boolean
  /** Drives to switch between; empty while confined. */
  roots: PathSegment[]
  segments: PathSegment[]
  entries: FileEntry[]
}

export class ApiError extends Error {
  status: number

  constructor(message: string, status: number) {
    super(message)
    this.name = 'ApiError'
    this.status = status
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  let response: Response
  try {
    response = await fetch(`/api${path}`, {
      headers: { 'Content-Type': 'application/json' },
      ...init,
    })
  } catch {
    // A network-level failure means the server is not running at all, which
    // is worth distinguishing from an error it deliberately returned.
    throw new ApiError('Cannot reach the rtsp-utils server', 0)
  }

  const text = await response.text()
  let body: unknown = null
  try {
    body = text ? JSON.parse(text) : null
  } catch {
    // Falling through to the status check gives a better message than a
    // parser error would.
  }

  if (!response.ok) {
    const message =
      (body as { error?: string } | null)?.error ??
      `Request failed with status ${response.status}`
    throw new ApiError(message, response.status)
  }

  return body as T
}

export function getHealth(): Promise<Health> {
  return request<Health>('/health')
}

export function listStreams(): Promise<Stream[]> {
  return request<{ streams: Stream[] }>('/streams').then((r) => r.streams)
}

export function addStream(input: {
  path: string
  name?: string
  start?: boolean
}): Promise<Stream> {
  return request<Stream>('/streams', {
    method: 'POST',
    body: JSON.stringify(input),
  })
}

export function startStream(name: string): Promise<Stream> {
  return request<Stream>(`/streams/${encodeURIComponent(name)}/start`, {
    method: 'POST',
  })
}

export function stopStream(name: string): Promise<Stream> {
  return request<Stream>(`/streams/${encodeURIComponent(name)}/stop`, {
    method: 'POST',
  })
}

export function removeStream(name: string): Promise<void> {
  return request<void>(`/streams/${encodeURIComponent(name)}`, {
    method: 'DELETE',
  })
}

/** Lists folders and media files inside the server's media directory. */
export function listFiles(path = ''): Promise<Listing> {
  return request<Listing>(`/files?path=${encodeURIComponent(path)}`)
}

/** URL of the live fragmented-MP4 preview for a running stream. */
export function previewUrl(name: string): string {
  return `/api/streams/${encodeURIComponent(name)}/preview.mp4`
}

/** URL of the still-image preview for a running JPEG stream. */
export function previewImageUrl(name: string): string {
  return `/api/streams/${encodeURIComponent(name)}/preview.jpg`
}
