// List animations with auto-animate. The CSS animations live in index.css.

import type { AutoAnimationPlugin } from '@formkit/auto-animate'
import { useAutoAnimate } from '@formkit/auto-animate/react'

const DURATION = 180
const EASE = 'cubic-bezier(.2, 0, 0, 1)'

// The library checks the reduced-motion setting only for its default animations, not for plugins.
const reducedMotion = () => window.matchMedia('(prefers-reduced-motion: reduce)').matches
const none = (el: Element) => new KeyframeEffect(el, [], { duration: 0 })

// Same as the library default, but without width and height animations, which cause layout work.
const listPlugin: AutoAnimationPlugin = (el, action, oldCoords, newCoords) => {
  if (reducedMotion()) return none(el)
  if (action === 'add') {
    return new KeyframeEffect(el, [{ opacity: 0, transform: 'translateY(-4px)' }, { opacity: 1, transform: 'none' }], {
      duration: DURATION,
      easing: EASE,
    })
  }
  if (action === 'remove') {
    return new KeyframeEffect(el, [{ opacity: 1 }, { opacity: 0, transform: 'scale(.98)' }], { duration: DURATION, easing: 'ease-in' })
  }
  const dx = (oldCoords?.left ?? 0) - (newCoords?.left ?? 0)
  const dy = (oldCoords?.top ?? 0) - (newCoords?.top ?? 0)
  if (dx === 0 && dy === 0) return none(el)
  return new KeyframeEffect(el, [{ transform: `translate(${dx}px, ${dy}px)` }, { transform: 'none' }], { duration: DURATION, easing: EASE })
}

// The call log gets a new row on each SSE event. Only the new row moves, so heavy traffic
// does not start one animation for every row. Old rows leave at once.
const callLogPlugin: AutoAnimationPlugin = (el, action) => {
  if (action === 'remove') return [none(el), { styleReset: false }]
  if (action === 'remain' || reducedMotion()) return none(el)
  return new KeyframeEffect(el, [{ opacity: 0, transform: 'translateY(-4px)' }, { opacity: 1, transform: 'none' }], {
    duration: DURATION,
    easing: EASE,
  })
}

/** Ref for a parent element. Its direct children fade in, fade out, and slide to new places. */
export function useListAnimation<T extends HTMLElement>() {
  return useAutoAnimate<T>(listPlugin)[0]
}

/** Ref for the call log `tbody`. */
export function useCallLogAnimation() {
  return useAutoAnimate<HTMLTableSectionElement>(callLogPlugin)[0]
}
