import { useState } from 'react'
import { CircleAlert, Clapperboard } from 'lucide-react'

import { AddStreamForm } from '@/components/add-stream-form'
import { ServerHeader } from '@/components/server-header'
import { StreamCard } from '@/components/stream-card'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Card, CardContent } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { TooltipProvider } from '@/components/ui/tooltip'
import { useStreams } from '@/hooks/use-streams'

export default function App() {
  const { streams, health, error, loading, refresh, applyStream } = useStreams()
  const [refreshing, setRefreshing] = useState(false)

  const manualRefresh = async () => {
    setRefreshing(true)
    await refresh()
    setRefreshing(false)
  }

  return (
    <TooltipProvider delayDuration={300}>
      <div className="min-h-screen bg-background">
        <ServerHeader
          health={health}
          connected={error === null}
          onRefresh={manualRefresh}
          refreshing={refreshing}
        />

        <main className="mx-auto max-w-5xl space-y-6 px-6 py-8">
          {error && (
            <Alert variant="destructive">
              <CircleAlert />
              <AlertTitle>Not connected to the server</AlertTitle>
              <AlertDescription>
                {error}. Start it with{' '}
                <code className="font-mono">rtsp-utils --api</code> and this page
                will reconnect on its own.
              </AlertDescription>
            </Alert>
          )}

          <AddStreamForm onAdded={refresh} />

          {loading ? (
            <LoadingList />
          ) : streams.length === 0 ? (
            <EmptyState />
          ) : (
            <div className="space-y-4">
              {streams.map((stream) => (
                <StreamCard
                  key={stream.name}
                  stream={stream}
                  onUpdated={applyStream}
                  onRemoved={refresh}
                />
              ))}
            </div>
          )}
        </main>
      </div>
    </TooltipProvider>
  )
}

function LoadingList() {
  return (
    <div className="space-y-4">
      {[0, 1].map((i) => (
        <Skeleton key={i} className="h-44 w-full rounded-xl" />
      ))}
    </div>
  )
}

function EmptyState() {
  return (
    <Card className="border-dashed">
      <CardContent className="flex flex-col items-center gap-3 py-14 text-center">
        <div className="flex size-11 items-center justify-center rounded-full bg-muted">
          <Clapperboard className="size-5 text-muted-foreground" />
        </div>
        <div className="space-y-1">
          <p className="font-medium">No streams yet</p>
          <p className="text-sm text-muted-foreground">
            Add a video file above and it will be published over RTSP.
          </p>
        </div>
      </CardContent>
    </Card>
  )
}
