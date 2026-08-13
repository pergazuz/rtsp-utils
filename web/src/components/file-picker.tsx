import { useCallback, useEffect, useState } from 'react'
import {
  ChevronRight,
  CornerLeftUp,
  Film,
  Folder,
  FolderOpen,
  HardDrive,
  Home,
  Image as ImageIcon,
  Loader2,
  TriangleAlert,
} from 'lucide-react'

import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@/components/ui/dialog'
import { ScrollArea } from '@/components/ui/scroll-area'
import { listFiles, type Listing } from '@/lib/api'
import { formatBytes } from '@/lib/format'
import { cn } from '@/lib/utils'

interface FilePickerProps {
  /** Called with the chosen file's absolute path on the server. */
  onPick: (path: string) => void
  children: React.ReactNode
}

export function FilePicker({ onPick, children }: FilePickerProps) {
  const [open, setOpen] = useState(false)
  const [listing, setListing] = useState<Listing | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const browse = useCallback(async (path: string) => {
    setLoading(true)
    try {
      setListing(await listFiles(path))
      setError(null)
    } catch (e) {
      // Keep the previous listing on screen: a folder that cannot be opened
      // should not strand the picker with nowhere to click.
      setError(e instanceof Error ? e.message : 'Could not read that folder')
    } finally {
      setLoading(false)
    }
  }, [])

  // Start where the server says each time the dialog opens.
  useEffect(() => {
    if (open) void browse('')
  }, [open, browse])

  const choose = (path: string) => {
    onPick(path)
    setOpen(false)
  }

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>{children}</DialogTrigger>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>Choose a file</DialogTitle>
          <DialogDescription>
            Files on the machine running rtsp-utils.
          </DialogDescription>
        </DialogHeader>

        {listing && (
          <div className="flex flex-wrap items-center gap-1.5">
            <Button
              variant="outline"
              size="sm"
              className="h-7 gap-1.5 px-2"
              onClick={() => browse('')}
              title={listing.start}
            >
              <Home className="size-3.5" />
              Home
            </Button>

            {listing.roots.map((root) => (
              <Button
                key={root.path}
                variant={
                  listing.path.startsWith(root.path) ? 'secondary' : 'ghost'
                }
                size="sm"
                className="h-7 gap-1.5 px-2 font-mono text-xs"
                onClick={() => browse(root.path)}
              >
                <HardDrive className="size-3.5" />
                {root.label}
              </Button>
            ))}
          </div>
        )}

        <Breadcrumbs listing={listing} onNavigate={browse} />

        {error && (
          <p className="flex items-start gap-2 rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-sm text-destructive">
            <TriangleAlert className="mt-0.5 size-4 shrink-0" />
            {error}
          </p>
        )}

        <ScrollArea className="h-80 rounded-md border">
          {loading && !listing ? (
            <div className="flex h-80 items-center justify-center">
              <Loader2 className="size-5 animate-spin text-muted-foreground" />
            </div>
          ) : (
            <ul className="divide-y">
              {listing?.parent && (
                <EntryRow
                  icon={CornerLeftUp}
                  label="Up one level"
                  muted
                  onSelect={() => browse(listing.parent!)}
                />
              )}

              {listing?.entries.map((entry) =>
                entry.directory ? (
                  <EntryRow
                    key={entry.path}
                    icon={Folder}
                    label={entry.name}
                    trailing={<ChevronRight className="size-4 opacity-50" />}
                    onSelect={() => browse(entry.path)}
                  />
                ) : (
                  <EntryRow
                    key={entry.path}
                    icon={/\.jpe?g$/i.test(entry.name) ? ImageIcon : Film}
                    label={entry.name}
                    trailing={
                      <span className="text-xs tabular-nums text-muted-foreground">
                        {formatBytes(entry.size)}
                      </span>
                    }
                    onSelect={() => choose(entry.path)}
                  />
                ),
              )}

              {listing?.entries.length === 0 && (
                <li className="flex flex-col items-center gap-2 py-16 text-center">
                  <FolderOpen className="size-5 text-muted-foreground" />
                  <p className="text-sm text-muted-foreground">
                    No folders or media files here
                  </p>
                </li>
              )}
            </ul>
          )}
        </ScrollArea>

        <p className="flex items-center gap-1.5 truncate text-xs text-muted-foreground">
          {listing?.truncated
            ? 'Showing the first 1000 entries in this folder.'
            : 'Only .mov, .mp4, .m4v, .jpg and .jpeg files are listed.'}
        </p>
      </DialogContent>
    </Dialog>
  )
}

interface EntryRowProps {
  icon: React.ComponentType<{ className?: string }>
  label: string
  trailing?: React.ReactNode
  muted?: boolean
  onSelect: () => void
}

function EntryRow({
  icon: Icon,
  label,
  trailing,
  muted,
  onSelect,
}: EntryRowProps) {
  return (
    <li>
      <button
        type="button"
        onClick={onSelect}
        className="flex w-full items-center gap-3 px-3 py-2.5 text-left text-sm hover:bg-accent focus-visible:bg-accent focus-visible:outline-none"
      >
        <Icon
          className={cn('size-4 shrink-0', muted && 'text-muted-foreground')}
        />
        <span className={cn('flex-1 truncate', muted && 'text-muted-foreground')}>
          {label}
        </span>
        {trailing}
      </button>
    </li>
  )
}

/**
 * The trail comes from the server, which knows how to split a path on the
 * platform it is actually running on.
 */
function Breadcrumbs({
  listing,
  onNavigate,
}: {
  listing: Listing | null
  onNavigate: (path: string) => void
}) {
  if (!listing) return null

  return (
    <div className="flex flex-wrap items-center gap-0.5 rounded-md border bg-muted/30 px-2 py-1.5">
      {listing.segments.map((segment, i) => (
        <span key={segment.path} className="flex items-center">
          {i > 0 && (
            <ChevronRight className="size-3.5 shrink-0 text-muted-foreground" />
          )}
          <Button
            variant="ghost"
            size="sm"
            className="h-6 px-1.5 text-xs font-normal"
            onClick={() => onNavigate(segment.path)}
          >
            {segment.label}
          </Button>
        </span>
      ))}
    </div>
  )
}
