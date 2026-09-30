import '../css/app.css'
import { createApp, type Component } from 'vue'

// Vue components on server-rendered pages: <div data-component="Counter"
// data-props='{"start":1}'></div> mounts components/Counter.vue there with
// those props. Each component loads only on pages that use it.
const components = import.meta.glob<{ default: Component }>('./components/*.vue')

for (const element of document.querySelectorAll<HTMLElement>('[data-component]')) {
  const name = element.dataset.component
  const load = components[`./components/${name}.vue`]
  if (!load) {
    console.error(`No component ${name} in resources/js/components`)
    continue
  }
  const props = JSON.parse(element.dataset.props ?? '{}')
  load().then((module) => createApp(module.default, props).mount(element))
}
