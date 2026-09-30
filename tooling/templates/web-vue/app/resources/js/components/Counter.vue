<script setup lang="ts">
import { ref } from 'vue'

const props = defineProps<{ start?: number }>()
const count = ref(props.start ?? 0)
</script>

<template>
  <button
    type="button"
    class="inline-flex h-12 items-center gap-2 rounded-full border border-black/10 px-6 font-mono text-sm hover:bg-neutral-50 dark:border-white/15 dark:hover:bg-neutral-900"
    @click="count++"
  >
    Vue component · clicked {{ count }}×
  </button>
</template>
