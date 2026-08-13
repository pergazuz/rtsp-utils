import { useEffect, useState } from 'react'
import {
  AudioLines,
  Clock,
  Eye,
  EyeOff,
  Loader2,
  Play,
  Repeat,
  Square,
  Trash2,
  Users,
  Video,
} from 'lucide-react'
import { toast } from 'sonner'

import { CopyButton } from '@/components/copy-button'
import { StreamPreview } from '@/components/stream-preview'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardFooter, CardHeader } from '@/components/ui/card'
import { Separator } from '@/components/ui/separator'
import { useElapsed } from '@/hooks/use-streams'
import {
  removeStream,
  startStream,
  stopStream,
  type Stream,
} from '@/lib/api'
import { formatDuration } from '@/lib/format'

interface StreamCardProps {
  stream: Stream
  onUpdated: (stream: Stream) => void
  onRemoved: () => void
}

export function StreamCard({ stream, onUpdated, onRemoved }: StreamCardProps) {
  const [pending, setPending] = useState<'toggle' | 'remove' | null>(null)
  const [showPreview, setShowPreview] = useState(false)
  const running = stream.state === 'running'
  const elapsed = useElapsed(stream.startedAt)

  // A preview of a stopped stream has nothing to show, so close it. Starting
  // the stream again leaves it closed until it is asked for.
  useEffect(() => {
    if (!running) setShowPreview(false)
  }, [running])

  const toggle = async () => {
    setPending('toggle')
    try {
      const updated = running
        ? await stopStream(stream.name)
        : await startStream(stream.name)
      onUpdated(updated)
      toast.success(running ? `Stopped ${stream.name}` : `Started ${stream.name}`)
    } catch (e) {
      toast.error(e instanceof Error ? e.message : 'The action failed')
    } finally {
      setPending(null)
    }
  }

  const remove = async () => {
    setPending('remove')
    try {
      await removeStream(stream.name)
      onRemoved()
      toast.success(`Removed ${stream.name}`)
    } catch (e) {
      toast.error(e instanceof Error ? e.message : 'The action failed')
      setPending(null)
    }
  }

  return (
    <Card className="gap-0 overflow-hidden py-0">
      <CardHeader className="flex flex-row items-start justify-between gap-4 space-y-0 px-5 py-4">
        <div className="min-w-0 space-y-1">
          <div className="flex items-center gap-2">
            <h3 className="truncate text-base font-semibold">{stream.name}</h3>
            <StateBadge running={running} />
          </div>
          <p
            className="truncate font-mono text-xs text-muted-foreground"
            title={stream.path}
          >
            {stream.path}
          </p>
        </div>

        <div className="flex shrink-0 items-center gap-4 text-xs text-muted-foreground">
          <Stat icon={Users} label={`${stream.viewers}`} title="Connected clients" />
          <Stat
            icon={Clock}
            label={
              elapsed === null ? '—' : formatDuration(elapsed)
            }
            title={running ? 'Uptime' : 'Not running'}
          />
        </div>
      </CardHeader>

      <Separator />

      <CardContent className="space-y-3 px-5 py-4">
        <div className="flex items-center gap-2 rounded-md border bg-muted/40 px-3 py-2">
          <code className="min-w-0 flex-1 truncate font-mono text-sm">
            {stream.url}
          </code>
          <CopyButton value={stream.url} label="Copy RTSP URL" />
        </div>

        <div className="flex flex-wrap items-center gap-x-4 gap-y-2 text-xs text-muted-foreground">
          {stream.tracks.map((track) => (
            <span key={track.control} className="flex items-center gap-1.5">
              {track.kind === 'video' ? (
                <Video className="size-3.5" />
              ) : (
                <AudioLines className="size-3.5" />
              )}
              <span className="font-medium text-foreground">{track.codec}</span>
              <span>{track.detail}</span>
            </span>
          ))}
          <span className="flex items-center gap-1.5">
            <Clock className="size-3.5" />
            {formatDuration(stream.durationSeconds)}
          </span>
          {stream.looping && (
            <span className="flex items-center gap-1.5">
              <Repeat className="size-3.5" />
              looping
            </span>
          )}
        </div>

        {showPreview && running && <StreamPreview stream={stream} />}
      </CardContent>

      <CardFooter className="flex items-center justify-between gap-3 border-t bg-muted/20 px-5 py-3">
        <div className="flex items-center gap-2">
          <Button
            onClick={toggle}
            disabled={pending !== null}
            variant={running ? 'secondary' : 'default'}
            className="min-w-28"
          >
            {pending === 'toggle' ? (
              <Loader2 className="animate-spin" />
            ) : running ? (
              <Square />
            ) : (
              <Play />
            )}
            {running ? 'Stop' : 'Start'}
          </Button>

          {/* Watching only makes sense while the stream is on air. */}
          <Button
            variant="outline"
            onClick={() => setShowPreview((shown) => !shown)}
            disabled={!running || !stream.previewable}
            title={
              stream.previewable
                ? undefined
                : 'This stream has nothing the browser can preview'
            }
          >
            {showPreview ? <EyeOff /> : <Eye />}
            {showPreview ? 'Hide' : 'Watch'}
          </Button>
        </div>

        <Button
          onClick={remove}
          disabled={pending !== null}
          variant="ghost"
          size="sm"
          className="text-muted-foreground hover:text-destructive"
        >
          {pending === 'remove' ? (
            <Loader2 className="animate-spin" />
          ) : (
            <Trash2 />
          )}
          Remove
        </Button>
      </CardFooter>
    </Card>
  )
}

function StateBadge({ running }: { running: boolean }) {
  if (!running) {
    return (
      <Badge variant="secondary" className="gap-1.5">
        <span className="size-1.5 rounded-full bg-muted-foreground" />
        Stopped
      </Badge>
    )
  }

  return (
    <Badge className="gap-1.5 border-emerald-600/20 bg-emerald-500/15 text-emerald-700 dark:text-emerald-400">
      {/* A pulsing dot reads as "live" faster than the word does. */}
      <span className="relative flex size-1.5">
        <span className="absolute inline-flex size-full animate-ping rounded-full bg-emerald-500 opacity-75" />
        <span className="relative inline-flex size-1.5 rounded-full bg-emerald-500" />
      </span>
      Live
    </Badge>
  )
}

interface StatProps {
  icon: React.ComponentType<{ className?: string }>
  label: string
  title: string
}

function Stat({ icon: Icon, label, title }: StatProps) {
  return (
    <span className="flex items-center gap-1.5 tabular-nums" title={title}>
      <Icon className="size-3.5" />
      {label}
    </span>
  )
}
