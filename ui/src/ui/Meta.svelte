<script lang="ts">
  import type { Row } from '$proto/Row';
  import type { State } from '$proto/State';
  import { clock, langName, t, when, word } from '../lib/words.svelte';

  // What a recording is, in one quiet line: in the list, and above its transcript.
  let { row, s }: { row: Row; s: State } = $props();
  const w = $derived(s.writing.find((x) => x.id === row.id));
</script>

<small>
  {when(row.started)} · {clock(row.length_ms)} · {row.spoken.map((l) => langName(l)).join(' · ')}
  {#if row.draft}<span class="tag" title={t('draft.tip')}>{t('draft')}</span>{/if}
  {#if row.audio}<span class="tag" title={t('audio.tip')}>{t('audio')}</span>{/if}
  {#if w}<progress value={w.done} max={w.of || 1}></progress>{word('step code', w.step)}{/if}
</small>
