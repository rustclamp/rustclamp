import '../css/app.css'
import { createElement, type ComponentType } from 'react'
import { createRoot } from 'react-dom/client'

// React components on server-rendered pages: <div data-component="Counter"
// data-props='{"start":1}'></div> renders components/Counter.tsx there with
// those props. Each component loads only on pages that use it.
const components = import.meta.glob<{ default: ComponentType<object> }>('./components/*.tsx')

for (const element of document.querySelectorAll<HTMLElement>('[data-component]')) {
  const name = element.dataset.component
  const load = components[`./components/${name}.tsx`]
  if (!load) {
    console.error(`No component ${name} in resources/js/components`)
    continue
  }
  const props = JSON.parse(element.dataset.props ?? '{}')
  load().then((module) => createRoot(element).render(createElement(module.default, props)))
}
