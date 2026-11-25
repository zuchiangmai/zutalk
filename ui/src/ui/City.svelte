<script lang="ts">
  import type { RecordingView } from '$proto/RecordingView';
  import { send, store } from '../lib/core.svelte';
  import { t, when, word } from '../lib/words.svelte';

  // A recording, filed to an event of a city: the city's address, one of its events (the one
  // nearest the recording until another is chosen), and its audio while it has some, which goes
  // unless turned off.
  let { open }: { open: RecordingView } = $props();
  const id = $derived(open.row.id), filed = $derived(open.filed), cal = $derived(store.city);
  const going = $derived(store.progress.file?.id === id ? store.progress.file : null);
  const away = (at: number | null) => (at === null ? Infinity : Math.abs(at - open.row.started));
  const near = $derived(cal?.events.reduce<(typeof cal.events)[number] | null>((a, e) => (a && away(a.start) <= away(e.start) ? a : e), null));
  let chosen = $state(''), audio = $state(true);
  const event = $derived(cal?.events.some((e) => e.path === chosen) ? chosen : (near?.path ?? ''));
</script>

<p>
  {#if going}<progress value={going.done} max={going.of || 1}></progress>{word('step code', going.step)}
  {:else if filed}<span class:flag={filed.state !== 'city.filed'}>{word('step code', filed.state || 'filing')} · {filed.title}{filed.audio ? ` · ${t('audio')}` : ''}</span>{/if}
  <input value={filed?.city ?? cal?.address ?? ''} placeholder={t('city')} aria-label={t('city')} title={t('city.tip')}
    onchange={(e) => send({ do: 'City', address: e.currentTarget.value })} />
  {#if cal}
    <select aria-label={t('city.event')} value={event} onchange={(e) => (chosen = e.currentTarget.value)}>
      {#each cal.events as e (e.path)}<option value={e.path}>{e.title}{e.start === null ? '' : ` · ${when(e.start)}`}</option>
      {:else}<option value="" disabled>{t('city.empty')}</option>{/each}
    </select>
    {#if open.row.audio}<button aria-pressed={audio} title={t('city.audio.tip')} onclick={() => (audio = !audio)}>{t('audio')}</button>{/if}
    <button class="primary" title={t('city.file.tip')} disabled={!event || !!going} onclick={() => send({ do: 'File', id, event, audio: audio && open.row.audio })}>{t('city.file')}</button>
  {/if}
</p>
