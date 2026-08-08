#!/usr/bin/env bun
//
// Quick local run, identical on macOS, Linux and Windows.
//
// The same command works everywhere:
//
//     bun run.mjs [options] [-- extra rtsp-utils args]
//
// run.sh, run.ps1 and run.cmd are one-line shims onto this file, so the
// platform-specific behaviour lives here and only here.

import { spawn, spawnSync } from 'node:child_process'
import { existsSync, readdirSync, statSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import process from 'node:process'

const ROOT = dirname(fileURLToPath(import.meta.url))
const WINDOWS = process.platform === 'win32'
const EXE = WINDOWS ? '.exe' : ''

const USAGE = `Quick local run for rtsp-utils.

USAGE:
    bun run.mjs [options] [-- extra rtsp-utils args]

OPTIONS:
    --file <PATH>       Publish a video on startup
    --media-dir <DIR>   Folder the file picker opens in (default: this folder)
    --api-addr <ADDR>   Control API address (default: 127.0.0.1:8080)
    --dev               Vite dev server with hot reload, on port 5173
    --debug             Build the debug profile, which compiles faster
    --rebuild-ui        Rebuild the web UI even if it looks current
    --no-open           Do not open a browser
    -h, --help          Show this help

EXAMPLES:
    bun run.mjs
    bun run.mjs --file clip.mov --media-dir ~/Movies
    bun run.mjs --dev
    bun run.mjs -- --name cam1 --host 192.168.1.20
`

// ---- arguments -------------------------------------------------------------

function parseArgs(argv) {
  const options = {
    file: null,
    mediaDir: null,
    apiAddr: '127.0.0.1:8080',
    dev: false,
    debug: false,
    rebuildUi: false,
    open: true,
    passthrough: [],
  }

  const takesValue = {
    '--file': 'file',
    '--media-dir': 'mediaDir',
    '--api-addr': 'apiAddr',
  }

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i]

    // Everything after a bare `--` belongs to rtsp-utils, untouched.
    if (arg === '--') {
      options.passthrough = argv.slice(i + 1)
      break
    }

    if (arg in takesValue) {
      const value = argv[++i]
      if (value === undefined) fail(`${arg} needs a value`)
      options[takesValue[arg]] = value
      continue
    }

    switch (arg) {
      case '--dev': options.dev = true; break
      case '--debug': options.debug = true; break
      case '--rebuild-ui': options.rebuildUi = true; break
      case '--no-open': options.open = false; break
      case '-h':
      case '--help': process.stdout.write(USAGE); process.exit(0)
      default: fail(`unknown option: ${arg}`)
    }
  }

  return options
}

function fail(message) {
  console.error(`error: ${message}\n`)
  process.stderr.write(USAGE)
  process.exit(2)
}

// ---- helpers ---------------------------------------------------------------

const step = (message) => console.log(`\x1b[36m==>\x1b[0m ${message}`)

/** Runs a command to completion, inheriting stdio. Exits on failure. */
function run(command, args, options = {}) {
  // Windows resolves `bun`/`cargo` through .cmd shims, which need a shell.
  const result = spawnSync(command, args, {
    stdio: 'inherit',
    cwd: options.cwd ?? ROOT,
    shell: WINDOWS,
  })
  if (result.error || result.status !== 0) {
    console.error(`\nerror: \`${command} ${args.join(' ')}\` failed`)
    process.exit(result.status ?? 1)
  }
}

function isInstalled(command) {
  const probe = spawnSync(WINDOWS ? 'where' : 'which', [command], {
    stdio: 'ignore',
    shell: WINDOWS,
  })
  return probe.status === 0
}

function require(command, hint) {
  if (!isInstalled(command)) {
    console.error(`error: ${command} is not installed.`)
    console.error(`  ${hint}`)
    process.exit(1)
  }
}

/** Newest modification time under a file or directory tree. */
function newestMtime(path) {
  if (!existsSync(path)) return 0
  const info = statSync(path)
  if (!info.isDirectory()) return info.mtimeMs

  let newest = info.mtimeMs
  for (const entry of readdirSync(path, { withFileTypes: true })) {
    newest = Math.max(newest, newestMtime(join(path, entry.name)))
  }
  return newest
}

/** True when a UI source file is newer than the last build. */
function uiIsStale() {
  const built = join(ROOT, 'web', 'dist', 'index.html')
  if (!existsSync(built)) return true

  const builtAt = statSync(built).mtimeMs
  return ['web/src', 'web/index.html', 'web/package.json'].some(
    (source) => newestMtime(join(ROOT, source)) > builtAt,
  )
}

function openBrowser(url) {
  // Give the listener a moment before pointing a browser at it.
  setTimeout(() => {
    const [command, args] = WINDOWS
      ? ['cmd', ['/c', 'start', '', url]]
      : process.platform === 'darwin'
        ? ['open', [url]]
        : ['xdg-open', [url]]
    try {
      spawn(command, args, { stdio: 'ignore', detached: true }).unref()
    } catch {
      // A headless machine has no browser to open; the URL is printed anyway.
    }
  }, 1500).unref?.()
}

// ---- main ------------------------------------------------------------------

const options = parseArgs(process.argv.slice(2))

require('cargo', 'Install Rust from https://rustup.rs')
require(
  'bun',
  WINDOWS
    ? 'Install Bun:  powershell -c "irm bun.sh/install.ps1 | iex"'
    : 'Install Bun:  curl -fsSL https://bun.sh/install | bash',
)

const web = join(ROOT, 'web')

if (!existsSync(join(web, 'node_modules'))) {
  step('Installing web dependencies')
  run('bun', ['install'], { cwd: web })
}

// In dev mode Vite serves the UI from source, so there is nothing to build.
if (!options.dev) {
  if (options.rebuildUi || uiIsStale()) {
    step('Building web UI')
    run('bun', ['run', 'build'], { cwd: web })
  } else {
    step('Web UI is up to date')
  }
}

const profile = options.debug ? 'debug' : 'release'
step(`Building rtsp-utils (${profile})`)
run('cargo', options.debug ? ['build'] : ['build', '--release'])

const binary = join(ROOT, 'target', profile, `rtsp-utils${EXE}`)

const serverArgs = []
if (options.file) serverArgs.push(options.file)
serverArgs.push('--api', options.apiAddr)
if (options.mediaDir) serverArgs.push('--media-dir', options.mediaDir)
// In dev mode Vite serves the page and proxies /api, so the server only needs
// to answer the API calls.
if (options.dev) serverArgs.push('--no-ui')
serverArgs.push(...options.passthrough)

console.log()

if (!options.dev) {
  if (options.open) openBrowser(`http://${options.apiAddr}`)
  const server = spawn(binary, serverArgs, { stdio: 'inherit', cwd: ROOT })
  server.on('exit', (code) => process.exit(code ?? 0))
} else {
  step('Starting the API, with the UI on http://localhost:5173')
  const server = spawn(binary, serverArgs, { stdio: 'inherit', cwd: ROOT })

  let serverDied = false
  server.on('exit', () => {
    serverDied = true
  })

  // A port clash kills the server immediately; starting Vite against a dead
  // backend would just produce a UI that cannot load anything.
  await new Promise((resolve) => setTimeout(resolve, 1200))
  if (serverDied) {
    console.error('\nerror: the server exited on startup; see the message above.')
    console.error('  Another copy may already be using the RTSP port.')
    process.exit(1)
  }

  const stopServer = () => {
    if (!serverDied) server.kill()
  }
  process.on('exit', stopServer)
  process.on('SIGINT', () => process.exit(130))
  process.on('SIGTERM', () => process.exit(143))

  if (options.open) openBrowser('http://localhost:5173')

  const vite = spawn('bun', ['dev'], {
    stdio: 'inherit',
    cwd: web,
    shell: WINDOWS,
  })
  vite.on('exit', (code) => {
    stopServer()
    process.exit(code ?? 0)
  })
}
