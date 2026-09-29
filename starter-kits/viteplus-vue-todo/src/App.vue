<script setup lang="ts">
import { computed, ref, watch } from 'vue'

type Task = { id: number; title: string; done: boolean; category: string }
type Filter = 'all' | 'open' | 'done'

const starterTasks: Task[] = [
  { id: 1, title: 'Sketch the first version', done: true, category: 'Project' },
  { id: 2, title: 'Build a tiny thing that works', done: false, category: 'Project' },
  { id: 3, title: 'Take a proper lunch break', done: false, category: 'Personal' },
]

function readTasks(): Task[] {
  try {
    const saved = localStorage.getItem('daymark-tasks')
    return saved ? JSON.parse(saved) as Task[] : starterTasks
  } catch {
    return starterTasks
  }
}

const tasks = ref<Task[]>(readTasks())
const draft = ref('')
const activeFilter = ref<Filter>('all')
const openCount = computed(() => tasks.value.filter((task) => !task.done).length)
const visibleTasks = computed(() => tasks.value.filter((task) => {
  if (activeFilter.value === 'open') return !task.done
  if (activeFilter.value === 'done') return task.done
  return true
}))

watch(tasks, (value) => localStorage.setItem('daymark-tasks', JSON.stringify(value)), { deep: true })

function addTask() {
  const title = draft.value.trim()
  if (!title) return
  tasks.value.unshift({ id: Date.now(), title, done: false, category: 'Inbox' })
  draft.value = ''
}

function removeTask(id: number) {
  tasks.value = tasks.value.filter((task) => task.id !== id)
}
</script>

<template>
  <div class="shell">
    <header class="topbar">
      <a class="brand" href="#" aria-label="Daymark home"><span class="brand-mark">d</span> daymark</a>
      <div class="topbar-note"><span class="live-dot"></span> a little more focus, every day</div>
      <button class="avatar" aria-label="Profile">A</button>
    </header>

    <main class="layout">
      <aside class="sidebar">
        <p class="side-label">WORKSPACE</p>
        <button class="nav-item selected"><span class="nav-icon">▦</span> My tasks <span class="nav-count">{{ openCount }}</span></button>
        <button class="nav-item"><span class="nav-icon">◷</span> Upcoming</button>
        <button class="nav-item"><span class="nav-icon">✓</span> Completed</button>
        <p class="side-label project-label">YOUR LISTS <button aria-label="Add list">+</button></p>
        <button class="nav-item"><span class="list-dot violet"></span> Project</button>
        <button class="nav-item"><span class="list-dot orange"></span> Personal</button>
        <button class="nav-item"><span class="list-dot green"></span> Inbox</button>
        <div class="sidebar-tip"><span>✳</span><p><strong>Small steps count.</strong><br />Pick one thing and get started.</p></div>
      </aside>

      <section class="content">
        <div class="date-line">MONDAY, SEPTEMBER 29</div>
        <div class="heading-row"><div><h1>Good morning, Alex<span>.</span></h1><p class="subtitle">A clear mind starts with a clear list.</p></div><button class="more-button" aria-label="More options">···</button></div>

        <div class="summary-card">
          <div class="summary-copy"><span class="summary-kicker">YOUR DAY AT A GLANCE</span><strong>{{ openCount }} things <span>on your plate</span></strong><span class="summary-sub">One step at a time. You’re doing great.</span></div>
          <div class="progress-ring" :style="{ '--progress': `${tasks.length ? ((tasks.length - openCount) / tasks.length) * 100 : 0}%` }"><span>{{ tasks.length ? Math.round(((tasks.length - openCount) / tasks.length) * 100) : 0 }}<small>%</small></span></div>
          <span class="sparkle">✳</span>
        </div>

        <div class="section-heading"><div><h2>Today <span class="task-total">{{ tasks.length }}</span></h2><p>Your priorities for today</p></div><button class="sort-button">↕ <span>Sort</span></button></div>

        <form class="add-task" @submit.prevent="addTask"><span class="add-plus">+</span><input v-model="draft" aria-label="New task" placeholder="Add a task, press enter…" /><button v-if="draft.trim()" class="add-submit" type="submit">Add</button><span v-else class="shortcut">↵</span></form>

        <div class="filters" aria-label="Filter tasks"><button v-for="filter in (['all', 'open', 'done'] as Filter[])" :key="filter" :class="{ active: activeFilter === filter }" @click="activeFilter = filter">{{ filter === 'all' ? 'All tasks' : filter === 'open' ? 'To do' : 'Completed' }}</button></div>

        <ul class="task-list">
          <li v-for="task in visibleTasks" :key="task.id" class="task-row" :class="{ completed: task.done }">
            <button class="check-button" :aria-label="task.done ? 'Mark incomplete' : 'Complete task'" @click="task.done = !task.done">{{ task.done ? '✓' : '' }}</button>
            <span class="task-title">{{ task.title }}</span><span class="category" :class="task.category.toLowerCase()">{{ task.category }}</span>
            <button class="delete-button" aria-label="Delete task" @click="removeTask(task.id)">×</button>
          </li>
          <li v-if="visibleTasks.length === 0" class="empty-state">Nothing here yet. Add a task above when you’re ready.</li>
        </ul>

        <footer class="content-footer"><span>Made for getting things done, gently.</span><span>☀ &nbsp;Take a breath</span></footer>
      </section>
    </main>
  </div>
</template>
