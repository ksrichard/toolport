import { useEffect, useState } from 'react'

type Health = 'checking' | 'ok' | 'down'

export default function App() {
  const [health, setHealth] = useState<Health>('checking')

  useEffect(() => {
    fetch('/api/health')
      .then((res) => setHealth(res.ok ? 'ok' : 'down'))
      .catch(() => setHealth('down'))
  }, [])

  return (
    <main>
      <h1>ToolPort</h1>
      <p>
        Backend:{' '}
        {health === 'checking' && 'checking…'}
        {health === 'ok' && <span className="ok">ok</span>}
        {health === 'down' && <span className="bad">not reachable</span>}
      </p>
    </main>
  )
}
