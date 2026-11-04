// Small pixel-art scenes in the free space of the page header. Only the Neon theme shows them.
// The animations are CSS keyframes in index.css (section "pixel scenes").

import { useEffect, useRef, useState, useSyncExternalStore, type ReactElement } from 'react'

const OFF_KEY = 'toolport.pixelScenes'
const SCENE_MS = 5_000
// `?pixels=demo` in the URL plays the scenes one after another, to check them.
const DEMO = new URLSearchParams(window.location.search).get('pixels') === 'demo'
const FIRST_DELAY: [number, number] = DEMO ? [500, 500] : [3_000, 6_000]
const DELAY: [number, number] = DEMO ? [1_000, 1_000] : [30_000, 60_000]
const MAX_LABEL = 10

type SceneName = 'delivery' | 'plug' | 'pingpong' | 'thinking' | 'timeout'
const SCENES: SceneName[] = ['delivery', 'plug', 'pingpong', 'thinking', 'timeout']

/** A tool call from the live stream. The next scene can show its tool name. */
export interface SceneHint {
  id: number
  tool: string
  failed: boolean
}

const between = ([min, max]: [number, number]) => min + Math.random() * (max - min)
const pick = <T,>(list: T[]) => list[Math.floor(Math.random() * list.length)]
const short = (name: string) => (name.length > MAX_LABEL ? `${name.slice(0, MAX_LABEL - 1)}…` : name)

function readOff(): boolean {
  try {
    return localStorage.getItem(OFF_KEY) === 'off'
  } catch {
    return false
  }
}

function saveOff() {
  try {
    localStorage.setItem(OFF_KEY, 'off')
  } catch {
    // Private windows can block storage. The scenes then stay off only until the page reloads.
  }
}

// App.tsx sets data-theme in an effect, after the render. An observer sees the change.
function subscribeTheme(onChange: () => void) {
  const observer = new MutationObserver(onChange)
  observer.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] })
  return () => observer.disconnect()
}
const isNeon = () => document.documentElement.dataset.theme === 'neon'

function subscribeMedia(onChange: () => void) {
  const query = window.matchMedia('(prefers-reduced-motion: reduce)')
  query.addEventListener('change', onChange)
  return () => query.removeEventListener('change', onChange)
}
const reducedMotion = () => window.matchMedia('(prefers-reduced-motion: reduce)').matches

function subscribeVisibility(onChange: () => void) {
  document.addEventListener('visibilitychange', onChange)
  return () => document.removeEventListener('visibilitychange', onChange)
}
const isVisible = () => !document.hidden

interface Playing {
  key: number
  scene: SceneName
  label: string | null
}

interface PixelScenesProps {
  /** The Live page Pause button. */
  paused?: boolean
  /** The newest tool call. Only the Live page sends it. */
  hint?: SceneHint | null
}

export function PixelScenes({ paused = false, hint = null }: PixelScenesProps) {
  const neon = useSyncExternalStore(subscribeTheme, isNeon)
  const reduced = useSyncExternalStore(subscribeMedia, reducedMotion)
  const visible = useSyncExternalStore(subscribeVisibility, isVisible)
  const [off, setOff] = useState(readOff)
  const [playing, setPlaying] = useState<Playing | null>(null)
  const played = useRef(0)
  const lastHint = useRef<number | null>(null)
  const hintRef = useRef(hint)
  useEffect(() => {
    hintRef.current = hint
  }, [hint])

  const active = neon && !reduced && visible && !paused && !off

  useEffect(() => {
    if (!active) return
    let timer: ReturnType<typeof setTimeout>
    const wait = () => {
      timer = setTimeout(play, between(played.current === 0 ? FIRST_DELAY : DELAY))
    }
    const play = () => {
      const h = hintRef.current
      let scene = pick(SCENES)
      let label: string | null = null
      // Use each call at most once, and only in half of the cases, so the scenes stay varied.
      if (h && h.id !== lastHint.current && Math.random() < 0.5) {
        lastHint.current = h.id
        scene = h.failed ? 'timeout' : pick<SceneName>(['delivery', 'pingpong'])
        label = short(h.tool)
      }
      played.current += 1
      setPlaying({ key: played.current, scene, label })
      timer = setTimeout(() => {
        setPlaying(null)
        wait()
      }, SCENE_MS)
    }
    wait()
    return () => {
      clearTimeout(timer)
      setPlaying(null)
    }
  }, [active])

  if (!neon || reduced || off) return null

  return (
    <div className={`px-strip${playing ? ' playing' : ''}`}>
      <div className="px-box">
        {playing && (
          <svg
            key={playing.key}
            className={`px-scene px-${playing.scene}`}
            viewBox="0 0 64 18"
            width="192"
            height="54"
            aria-hidden="true"
            focusable="false"
          >
            <Scene name={playing.scene} label={playing.label} />
          </svg>
        )}
        <button
        className="px-close"
        type="button"
        aria-label="Turn off the header animations"
        title="Turn off the header animations"
        onClick={() => {
          saveOff()
          setOff(true)
        }}
      >
          ×
        </button>
      </div>
    </div>
  )
}

/**
 * One sprite from a text grid. Each character is one art pixel:
 * `#` body, `+` light, `.` empty. A run of the same character becomes one rect.
 */
function Sprite({ rows, x = 0, y = 0, className }: { rows: string[]; x?: number; y?: number; className?: string }) {
  const rects: ReactElement[] = []
  rows.forEach((row, r) => {
    let c = 0
    while (c < row.length) {
      const ch = row[c]
      let end = c
      while (end < row.length && row[end] === ch) end++
      if (ch !== '.') {
        rects.push(<rect key={`${r}-${c}`} x={x + c} y={y + r} width={end - c} height={1} className={ch === '+' ? 'px-lit' : undefined} />)
      }
      c = end
    }
  })
  return <g className={className}>{rects}</g>
}

// An original robot: a square head with an antenna, a box body, and two legs.
const HEAD = ['....+....', '....#....', '.#######.', '.##.#.##.', '.#######.', '.##...##.', '.#######.']
const HEAD_SLEEP = ['....+....', '....#....', '.#######.', '.#######.', '.##.#.##.', '.#######.', '.#######.']
const BODY = ['..#####..', '#.#####.#', '#.##+##.#', '..#####..']
const LEGS_A = ['..#...#..']
const LEGS_B = ['.#.....#.']

/** The robot is 9 x 12 art pixels. Its feet stand on the ground line. */
function Bot({ walk = false, sleep = false }: { walk?: boolean; sleep?: boolean }) {
  return (
    <g>
      <Sprite rows={HEAD} y={0} className={sleep ? 'px-eyes-open' : undefined} />
      {sleep && <Sprite rows={HEAD_SLEEP} y={0} className="px-eyes-shut" />}
      <Sprite rows={BODY} y={7} />
      {walk ? (
        <>
          <Sprite rows={LEGS_A} y={11} className="px-legs-a" />
          <Sprite rows={LEGS_B} y={11} className="px-legs-b" />
        </>
      ) : (
        <Sprite rows={LEGS_A} y={11} />
      )}
    </g>
  )
}

const PACKET = ['####', '#++#', '####']
const SOCKET = ['######', '#....#', '#.##.#', '#....#', '#.##.#', '#....#', '######']
const SERVER = ['#######', '#.+...#', '#######', '#.+...#', '#######', '#.+...#', '#######']
const PLUG = ['.....###..', '#########.', '.....#####', '#########.', '.....###..']
const SPARK = ['#.#.#', '.###.', '##+##', '.###.', '#.#.#']

interface LabelProps {
  x: number
  y: number
  text: string
  className?: string
  anchor?: 'start' | 'middle' | 'end'
}

function Label({ x, y, text, className, anchor }: LabelProps) {
  return (
    <text x={x} y={y} textAnchor={anchor} className={`px-text${className ? ` ${className}` : ''}`}>
      {text}
    </text>
  )
}

function Scene({ name, label }: { name: SceneName; label: string | null }) {
  switch (name) {
    case 'delivery':
      return (
        <>
          <g transform="translate(55 11)">
            <Sprite rows={SOCKET} />
          </g>
          <Label x={54} y={9} text="MCP" />
          <g transform="translate(0 6)">
            <g className="px-walk">
              <Bot walk />
              <g className="px-drop">
                <Sprite rows={PACKET} x={10} y={8} />
                <Label x={14} y={-1} text={label ?? '{json}'} anchor="end" />
              </g>
            </g>
          </g>
          <Sprite rows={SPARK} x={49} y={8} className="px-spark" />
          <Label x={30} y={6} text="200 OK" className="px-ok" />
        </>
      )
    case 'plug':
      return (
        <>
          <g className="px-plug-l">
            <rect x={0} y={12} width={14} height={1} />
            <Sprite rows={PLUG} x={14} y={10} />
          </g>
          <g className="px-plug-r">
            <Sprite rows={SOCKET} x={40} y={9} />
            <rect x={46} y={12} width={18} height={1} />
          </g>
          <Label x={18} y={6} text="CONNECTED" className="px-ok" />
        </>
      )
    case 'pingpong':
      return (
        <>
          <g transform="translate(2 6)">
            <g className="px-hop">
              <Bot />
            </g>
          </g>
          <g transform="translate(54 11)">
            <Sprite rows={SERVER} />
          </g>
          <g className="px-ball">
            <Sprite rows={PACKET} x={12} y={12} />
            {label && <Label x={14} y={10} text={label} anchor="middle" className="px-ball-tag" />}
          </g>
        </>
      )
    case 'thinking':
      return (
        <>
          <g transform="translate(10 6)">
            <Bot />
          </g>
          <rect x={21} y={7} width={1} height={1} className="px-dot1" />
          <rect x={23} y={5} width={2} height={2} className="px-dot2" />
          <rect x={26} y={2} width={2} height={2} className="px-dot3" />
          <g className="px-scroll">
            <rect x={30} y={1} width={30} height={16} className="px-paper" />
            <rect x={30} y={1} width={1} height={16} />
            <rect x={59} y={1} width={1} height={16} />
            <Label x={32} y={6} text="tools/list" className="px-ink" />
            <rect x={32} y={9} width={20} height={1} className="px-line" />
            <rect x={32} y={12} width={14} height={1} className="px-line" />
            <rect x={32} y={15} width={17} height={1} className="px-line" />
          </g>
        </>
      )
    case 'timeout':
      return (
        <>
          <g transform="translate(8 6)">
            <g className="px-hop">
              <Bot sleep />
            </g>
          </g>
          <Label x={19} y={6} text="z" className="px-z1" />
          <Label x={22} y={4} text="z" className="px-z2" />
          <Label x={25} y={2} text="Z" className="px-z3" />
          <g className="px-late">
            <Sprite rows={PACKET} x={30} y={14} />
            {label && <Label x={32} y={12} text={label} anchor="middle" />}
          </g>
          <Label x={19} y={8} text="!" className="px-alert" />
        </>
      )
  }
}
