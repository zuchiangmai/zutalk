<script lang="ts">
  import { t } from '../lib/words.svelte';

  // What cannot be undone is asked once more, in place: a second press does it.
  let { label, tip, act }: { label: string; tip?: string; act: () => void } = $props();
  let sure = $state(false);
</script>

<button class="danger" title={tip} onblur={() => (sure = false)} onclick={() => (sure ? ((sure = false), act()) : (sure = true))}>
  {sure ? t('confirm', label) : label}
</button>
