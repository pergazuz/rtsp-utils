import { useCallback, useEffect, useRef, useState } from 'react'

import { getHealth, listStreams, type Health, type Stream } from '@/lib/api'

const POLL_INTERVAL_MS = 2000

interface UseStreams {
  streams: Stream[]
  health: Health | null
  /** Set while the server is unreachable or the last poll failed. */
  error: string | null
  loading: boolean
  refresh: () => Promise<void>
  /** Applies a server response for one stream without waiting for a poll. */
  applyStream: (stream: Stream) => void
}

/**
 * Keeps the stream list in step with the server by polling. Viewer counts and
 * uptime change without any action from this browser, so a push-free poll is
 * both simpler and sufficient at this cadence.
 */
export function useStreams(): UseStreams {
  const [streams, setStreams] = useState<Stream[]>([])
  const [health, setHealth] = useState<Health | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)

  // Guards against a slow response landing after a newer one.
  const requestId = useRef(0)

  const refresh = useCallback(async () => {
    const id = ++requestId.current
    try {
      const [nextStreams, nextHealth] = await Promise.all([
        listStreams(),
        getHealth(),
      ])
      if (id !== requestId.current) return
      setStreams(nextStreams)
      setHealth(nextHealth)
      setError(null)
    } catch (e) {
      if (id !== requestId.current) return
      setError(e instanceof Error ? e.message : 'Something went wrong')
    } finally {
      if (id === requestId.current) setLoading(false)
    }
  }, [])

  useEffect(() => {
    void refresh()

    const interval = setInterval(() => {
      // No point polling a tab nobody is looking at.
      if (document.visibilityState === 'visible') void refresh()
    }, POLL_INTERVAL_MS)

    const onVisible = () => {
      if (document.visibilityState === 'visible') void refresh()
    }
    document.addEventListener('visibilitychange', onVisible)

    return () => {
      clearInterval(interval)
      document.removeEventListener('visibilitychange', onVisible)
    }
  }, [refresh])

  const applyStream = useCallback((updated: Stream) => {
    setStreams((current) =>
      current.map((stream) =>
        stream.name === updated.name ? updated : stream,
      ),
    )
  }, [])

  return { streams, health, error, loading, refresh, applyStream }
}

/** Seconds elapsed since `since`, ticking once a second. Null when stopped. */
export function useElapsed(since: number | null): number | null {
  const [now, setNow] = useState(() => Date.now())

  useEffect(() => {
    if (since === null) return
    const interval = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(interval)
  }, [since])

  if (since === null) return null
  return Math.max(0, Math.floor((now - since) / 1000))
}
