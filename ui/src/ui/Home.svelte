<script lang="ts">
  import type { Setup } from '$proto/Setup';
  import type { Sound } from '$proto/Sound';
  import type { State } from '$proto/State';
  import { pick, send } from '../lib/core.svelte';
  import { t, word } from '../lib/words.svelte';
  import Langs from './Langs.svelte';
  import Meta from './Meta.svelte';
  import Room from './Room.svelte';

  let { s }: { s: State } = $props();
  let query = $state('');
  const found = $derived(new Set(s.hits.map(([id]) => id)));
  const rows = $derived(s.library.filter((r) => !query || found.has(r.id)));
  const set = (patch: Partial<Setup>) => send({ do: 'Setup', setup: { ...s.setup, ...patch } });
  const sounds: Sound[] = ['Mic', 'System', 'Both'];
  const live = $derived(s.rooms.find((r) => r.recording === null));
  const take = () => pick().then((path) => void (path && send({ do: 'Import', path })));
</script>

<section class="start">
  <p>
    <button class="primary" disabled={!!s.session} onclick={() => send({ do: 'Start' })}>{t('start')}</button>
    <button disabled={!!s.session} onclick={take}>{t('import')}</button>
  </p>
  <Langs label={t('setup.spoken')} all={s.languages} list={s.setup.spoken} set={(spoken) => set({ spoken })} />
  <Langs label={t('setup.shown')} all={s.languages} list={s.setup.shown} set={(shown) => set({ shown })} />
  <p>
    <select aria-label={t('sound')} value={s.setup.sound} onchange={(e) => set({ sound: e.currentTarget.value as Sound })}>
      {#each sounds as v (v)}<option value={v}>{t(`sound.${v}`)}</option>{/each}
    </select>
    {#if s.setup.sound !== 'System'}
      <select aria-label={t('device')} value={s.setup.device ?? ''} onchange={(e) => set({ device: e.currentTarget.value || null })}>
        <option value="">{t('device.default')}</option>
        {#each s.devices as d (d.id)}<option value={d.id}>{d.name}</option>{/each}
      </select>
    {/if}
  </p>
  {#if live}<Room room={live} />{/if}
</section>
{#if s.moving}
  <p class="quiet"><progress value={s.moving.done} max={s.moving.of || 1}></progress> {word('step code', s.moving.step)} · {s.moving.done}/{s.moving.of}</p>
{/if}
{#each s.unmoved as [title, code], i (i)}<p class="quiet">{t('unmoved')} · {title || t('untitled')} · {word('code', code)}</p>{/each}
<input type="search" placeholder={t('search')} aria-label={t('search')} bind:value={query} oninput={() => send({ do: 'Search', query })} />
<ul>
  {#each rows as row (row.id)}
    <li><button onclick={() => send({ do: 'Open', id: row.id })}>
      <b class:quiet={!row.title}>{row.title || row.preview || t('untitled')}</b><Meta {row} {s} />
    </button></li>
  {:else}
    <li class="quiet">{t(query ? 'nomatch' : 'empty')}</li>
  {/each}
</ul>
