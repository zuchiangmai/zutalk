<script lang="ts">
  import { langName, t } from '../lib/words.svelte';

  // A list of languages: × takes one away, + adds one the core has rules for.
  let { label, all, list, set }: { label: string; all: string[]; list: string[]; set: (next: string[]) => void } = $props();
</script>

<p>
  <span class="quiet">{label}</span>
  {#each list as l (l)}
    <button class="on" aria-label={t('remove.one', langName(l))} onclick={() => set(list.filter((x) => x !== l))}>{langName(l)} ×</button>
  {/each}
  <select aria-label={t('lang.add', label)} value="" onchange={(e) => (set([...list, e.currentTarget.value]), (e.currentTarget.value = ''))}>
    <option value="" disabled>+</option>
    {#each all.filter((l) => !list.includes(l)) as l (l)}<option value={l}>{langName(l)}</option>{/each}
  </select>
</p>
