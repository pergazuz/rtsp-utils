import { RefreshCw, Radio } from 'lucide-react'

import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'
import type { Health } from '@/lib/api'

interface ServerHeaderProps {
  health: Health | null
  connected: boolean
  onRefresh: () => void
  refreshing: boolean
}

export function ServerHeader({
  health,
  connected,
  onRefresh,
  refreshing,
}: ServerHeaderProps) {
  return (
    <header className="border-b bg-card/50">
      <div className="mx-auto flex max-w-5xl items-center justify-between gap-4 px-6 py-5">
        <div className="flex items-center gap-3">
          <div className="flex size-9 items-center justify-center rounded-lg bg-primary text-primary-foreground">
            <Radio className="size-4.5" />
          </div>
          <div>
            <h1 className="text-lg leading-tight font-semibold">rtsp-utils</h1>
            <p className="text-xs text-muted-foreground">
              {health
                ? `RTSP on ${health.rtsp.bind} · v${health.version}`
                : 'Publish local video files as live RTSP streams'}
            </p>
          </div>
        </div>

        <div className="flex items-center gap-3">
          <span className="flex items-center gap-2 text-xs text-muted-foreground">
            <span
              className={cn(
                'size-2 rounded-full',
                connected ? 'bg-emerald-500' : 'bg-destructive',
              )}
            />
            {connected ? 'Connected' : 'Offline'}
          </span>

          <Button
            variant="outline"
            size="sm"
            onClick={onRefresh}
            disabled={refreshing}
          >
            <RefreshCw className={cn(refreshing && 'animate-spin')} />
            Refresh
          </Button>
        </div>
      </div>
    </header>
  )
}
