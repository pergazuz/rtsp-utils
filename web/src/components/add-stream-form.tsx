import { useState, type FormEvent } from 'react'
import { Film, FolderOpen, Loader2, Plus, X } from 'lucide-react'
import { toast } from 'sonner'

import { FilePicker } from '@/components/file-picker'
import { Button } from '@/components/ui/button'
import { Card, CardContent } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { addStream } from '@/lib/api'

interface AddStreamFormProps {
  onAdded: () => void
}

export function AddStreamForm({ onAdded }: AddStreamFormProps) {
  const [path, setPath] = useState('')
  const [name, setName] = useState('')
  const [submitting, setSubmitting] = useState(false)

  const submit = async (event: FormEvent) => {
    event.preventDefault()
    if (!path || submitting) return

    setSubmitting(true)
    try {
      const stream = await addStream({
        path,
        name: name.trim() || undefined,
      })
      setPath('')
      setName('')
      onAdded()
      toast.success(`Publishing ${stream.name}`, { description: stream.url })
    } catch (e) {
      toast.error(e instanceof Error ? e.message : 'Could not add the file')
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <Card>
      <CardContent>
        <form onSubmit={submit} className="flex flex-col gap-4 sm:flex-row sm:items-end">
          <div className="min-w-0 flex-1 space-y-2">
            <Label>Video file</Label>
            {path ? (
              <SelectedFile path={path} onClear={() => setPath('')} />
            ) : (
              <FilePicker onPick={setPath}>
                <Button
                  type="button"
                  variant="outline"
                  className="w-full justify-start font-normal text-muted-foreground"
                >
                  <FolderOpen />
                  Browse for a video…
                </Button>
              </FilePicker>
            )}
          </div>

          <div className="space-y-2 sm:w-48">
            <Label htmlFor="name">
              Stream name{' '}
              <span className="font-normal text-muted-foreground">optional</span>
            </Label>
            <Input
              id="name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="from the file name"
              autoComplete="off"
              spellCheck={false}
              className="font-mono text-sm"
            />
          </div>

          <Button type="submit" disabled={!path || submitting}>
            {submitting ? <Loader2 className="animate-spin" /> : <Plus />}
            Add stream
          </Button>
        </form>
      </CardContent>
    </Card>
  )
}

function SelectedFile({
  path,
  onClear,
}: {
  path: string
  onClear: () => void
}) {
  return (
    <div className="flex h-9 items-center gap-2 rounded-md border bg-muted/40 px-3">
      <Film className="size-4 shrink-0 text-muted-foreground" />
      <span className="min-w-0 flex-1 truncate font-mono text-sm" title={path}>
        {path}
      </span>
      <Button
        type="button"
        variant="ghost"
        size="icon"
        className="size-6 shrink-0"
        onClick={onClear}
        aria-label="Choose a different file"
      >
        <X className="size-3.5" />
      </Button>
    </div>
  )
}
