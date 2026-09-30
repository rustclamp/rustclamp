import { useState } from 'react'

export default function Counter({ start = 0 }: { start?: number }) {
  const [count, setCount] = useState(start)
  return (
    <button
      type="button"
      className="inline-flex h-12 items-center gap-2 rounded-full border border-black/10 px-6 font-mono text-sm hover:bg-neutral-50 dark:border-white/15 dark:hover:bg-neutral-900"
      onClick={() => setCount(count + 1)}
    >
      React component · clicked {count}×
    </button>
  )
}
