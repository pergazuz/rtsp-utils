import { useEffect, useRef, useState } from 'react'
import { Loader2, MonitorPlay, TriangleAlert } from 'lucide-react'

import { previewUrl, type Stream } from '@/lib/api'

/**
 * How far ahead of the playhead the buffer may run before we skip forward.
 * A live view that drifts behind is worse than one that jumps.
 */
const MAX_LATENCY_SECONDS = 2.5

/** Keep roughly this much history buffered; the rest is evicted. */
const KEEP_BUFFERED_SECONDS = 12

type State = 'connecting' | 'playing' | 'error'

interface StreamPreviewProps {
  stream: Stream
}

/**
 * Plays the server's fragmented-MP4 rendering of the stream through Media
 * Source Extensions. This is video only: it is a monitor, not a player.
 */
export function StreamPreview({ stream }: StreamPreviewProps) {
  const videoRef = useRef<HTMLVideoElement>(null)
  const [state, setState] = useState<State>('connecting')
  const [message, setMessage] = useState<string | null>(null)

  const codec =
    stream.tracks.find((t) => t.codecString)?.codecString ?? 'avc1.42e01e'

  useEffect(() => {
    const video = videoRef.current
    if (!video) return

    const mimeType = `video/mp4; codecs="${codec}"`
    if (!('MediaSource' in window) || !MediaSource.isTypeSupported(mimeType)) {
      setState('error')
      setMessage(`This browser cannot play ${codec}`)
      return
    }

    const controller = new AbortController()
    const mediaSource = new MediaSource()
    const objectUrl = URL.createObjectURL(mediaSource)
    video.src = objectUrl

    // Fragments arrive faster than they can be appended, so they queue.
    const queue: Uint8Array<ArrayBuffer>[] = []
    let sourceBuffer: SourceBuffer | null = null
    let ended = false

    const pump = () => {
      if (!sourceBuffer || sourceBuffer.updating || queue.length === 0) return
      try {
        sourceBuffer.appendBuffer(queue.shift()!)
      } catch {
        // A full buffer throws; the eviction below frees room for the retry.
        evict()
      }
    }

    const evict = () => {
      if (!sourceBuffer || sourceBuffer.updating) return
      const buffered = sourceBuffer.buffered
      if (buffered.length === 0) return
      const start = buffered.start(0)
      const cutoff = video.currentTime - KEEP_BUFFERED_SECONDS
      if (cutoff > start) {
        try {
          sourceBuffer.remove(start, cutoff)
        } catch {
          // Removal races with an append; the next tick tries again.
        }
      }
    }

    const onSourceOpen = async () => {
      URL.revokeObjectURL(objectUrl)
      try {
        sourceBuffer = mediaSource.addSourceBuffer(mimeType)
        sourceBuffer.mode = 'segments'
        sourceBuffer.addEventListener('updateend', pump)
      } catch (e) {
        setState('error')
        setMessage(e instanceof Error ? e.message : 'Cannot open a video buffer')
        return
      }

      try {
        const response = await fetch(previewUrl(stream.name), {
          signal: controller.signal,
        })
        if (!response.ok || !response.body) {
          const detail = await response.json().catch(() => null)
          throw new Error(detail?.error ?? `Preview failed (${response.status})`)
        }

        const reader = response.body.getReader()
        for (;;) {
          const { done, value } = await reader.read()
          if (done || ended) break
          // A fetch body is never backed by a SharedArrayBuffer, which is the
          // only case the wider type is guarding against.
          queue.push(value as Uint8Array<ArrayBuffer>)
          pump()
        }
      } catch (e) {
        if (controller.signal.aborted) return
        setState('error')
        setMessage(e instanceof Error ? e.message : 'The preview stopped')
      }
    }

    mediaSource.addEventListener('sourceopen', onSourceOpen, { once: true })

    // Keep the playhead near the live edge rather than letting it drift.
    const chase = setInterval(() => {
      if (video.buffered.length === 0) return
      const live = video.buffered.end(video.buffered.length - 1)
      if (live - video.currentTime > MAX_LATENCY_SECONDS) {
        video.currentTime = live - 0.3
      }
      evict()
    }, 2000)

    const onPlaying = () => setState('playing')
    video.addEventListener('playing', onPlaying)

    return () => {
      ended = true
      controller.abort()
      clearInterval(chase)
      video.removeEventListener('playing', onPlaying)
      sourceBuffer?.removeEventListener('updateend', pump)
      // Detaching the source stops the decoder and releases the socket.
      video.removeAttribute('src')
      video.load()
    }
  }, [stream.name, codec])

  return (
    <div className="relative aspect-video w-full overflow-hidden rounded-md border bg-black">
      <video
        ref={videoRef}
        autoPlay
        muted
        playsInline
        className="size-full object-contain"
      />

      {state !== 'playing' && (
        <div className="absolute inset-0 flex flex-col items-center justify-center gap-2 bg-black/70 text-center text-sm text-white/80">
          {state === 'error' ? (
            <>
              <TriangleAlert className="size-5 text-amber-400" />
              <p className="max-w-xs px-4">{message}</p>
            </>
          ) : (
            <>
              <Loader2 className="size-5 animate-spin" />
              <p>Connecting…</p>
            </>
          )}
        </div>
      )}

      {state === 'playing' && (
        <div className="pointer-events-none absolute left-2 top-2 flex items-center gap-1.5 rounded bg-black/60 px-2 py-1 text-[11px] font-medium text-white">
          <MonitorPlay className="size-3" />
          Live preview · video only
        </div>
      )}
    </div>
  )
}
