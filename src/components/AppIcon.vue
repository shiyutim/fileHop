<script setup lang="ts">
withDefaults(defineProps<{ name: string; size?: number }>(), { size: 20 });
const paths: Record<string, string[]> = {
  transfer: ['M4 7h16m-4-4 4 4-4 4', 'M20 17H4m4-4-4 4 4 4'],
  history: ['M3 11a9 9 0 1 1 2.8 7', 'M3 4v7h7', 'M12 7v5l3 2'],
  settings: [
    'M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8',
    'm9 3-.5 2a8 8 0 0 0-1.7 1L4.9 5.4 2 10l1.5 1.4a8 8 0 0 0 0 2L2 14.8l2.9 4.6 1.9-.6a8 8 0 0 0 1.7 1l.5 2.2h6l.5-2.2a8 8 0 0 0 1.7-1l1.9.6 2.9-4.6-1.5-1.4a8 8 0 0 0 0-2L22 10l-2.9-4.6-1.9.6a8 8 0 0 0-1.7-1L15 3Z',
  ],
  laptop: ['M5 4h14a1 1 0 0 1 1 1v11H4V5a1 1 0 0 1 1-1Z', 'm4 16-2 4h20l-2-4', 'M10 17h4'],
  monitor: ['M3 4h18v13H3z', 'M8 21h8m-4-4v4'],
  plus: ['M12 5v14M5 12h14'],
  arrow: ['M4 12h16m-6-6 6 6-6 6'],
  arrowUp: ['M12 20V4m-6 6 6-6 6 6'],
  arrowDown: ['M12 4v16m-6-6 6 6 6-6'],
  chevron: ['m9 5 7 7-7 7'],
  file: ['M14 3H5v18h14V8l-5-5Z', 'M14 3v5h5', 'M8 12h8m-8 4h5'],
  image: ['M4 3h16v18H4z', 'm4 17 5-5 3 3 3-4 5 6', 'M8 7h.01'],
  archive: ['M4 3h16v18H4z', 'M11 3v2h2v2h-2v2h2v2h-2v2', 'M10 15h4v3h-4z'],
  folder: ['M3 6h7l2 2h9v12H3z', 'M3 6V4h6l2 2'],
  folderOpen: ['M3 10V5h6l2 2h9v3', 'M2 10h20l-3 10H5z'],
  close: ['m6 6 12 12M6 18 18 6'],
  check: ['m5 12 4 4L19 6'],
  shield: ['m12 3 8 3v6c0 5-8 9-8 9s-8-4-8-9V6z', 'm8 12 3 3 5-6'],
  wifi: ['M2 8a16 16 0 0 1 20 0', 'M5 12a11 11 0 0 1 14 0', 'M8 16a6 6 0 0 1 8 0', 'M12 20h.01'],
  link: [
    'm10 8 2-2a5 5 0 0 1 7 7l-3 3a5 5 0 0 1-7 0',
    'm14 16-2 2a5 5 0 0 1-7-7l3-3a5 5 0 0 1 7 0',
  ],
  refresh: ['M20 7a9 9 0 0 0-15-2L2 8', 'M2 2v6h6', 'M4 17a9 9 0 0 0 15 2l3-3', 'M22 22v-6h-6'],
  copy: ['M9 9h12v12H9z', 'M15 5V3H3v12h2'],
  info: ['M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18', 'M12 11v6m0-10h.01'],
  trash: ['M3 6h18M9 6V3h6v3', 'm5 6 1 15h12l1-15', 'M10 10v7m4-7v7'],
  alert: ['m12 3 10 18H2Z', 'M12 9v5m0 3h.01'],
};
</script>
<template>
  <svg
    :width="size"
    :height="size"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="1.65"
    stroke-linecap="round"
    stroke-linejoin="round"
    aria-hidden="true"
  >
    <path v-for="(path, index) in paths[name] || paths.file" :key="index" :d="path" />
  </svg>
</template>
