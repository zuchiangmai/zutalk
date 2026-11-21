<script lang="ts">
  import { seen, send, store } from '../lib/core.svelte';
  import { t, word } from '../lib/words.svelte';
  import Home from './Home.svelte';
  import Live from './Live.svelte';
  import Reader from './Reader.svelte';
  import Settings from './Settings.svelte';

  // What is on screen. An open recording is the core's; the rest is where the person went.
  let page = $state<'home' | 'live' | 'settings'>('home');
  const s = $derived(store.state);
  let was: string | undefined;
  $effect(() => {
    const id = s?.session?.id;
    if (id !== was) page = id ? 'live' : 'home';
    was = id;
  });
  const pages = $derived<(typeof page)[]>(s?.session ? ['home', 'live', 'settings'] : ['home', 'settings']);
  const go = (to: typeof page) => ((page = to), s?.open && send({ do: 'Close' }));
  // Each notice is said once, for a moment; one that can be undone stays a little longer.
  const top = $derived(store.notices[0]);
  $effect(() => {
    if (!top) return;
    const n = top.n, id = setTimeout(() => seen(n), top.undo ? 8000 : 5000);
    return () => clearTimeout(id);
  });
</script>

{#if s}
  <header>
    <b>ZuTalk</b>
    {#each pages as p (p)}
      <button aria-current={p === page && !s.open ? 'page' : undefined} onclick={() => go(p)}>{t(`nav.${p}`)}</button>
    {/each}
    <!-- Installing relaunches the app, so it is not offered while recording. -->
    {#if s.update && !s.session}<button class="primary" title={s.update} onclick={() => send({ do: 'Update' })}>{t('update')}</button>{/if}
  </header>
  <main>
    {#if s.open}<Reader {s} open={s.open} />
    {:else if page === 'live' && s.session}<Live {s} session={s.session} />
    {:else if page === 'settings'}<Settings {s} />
    {:else}<Home {s} />{/if}
  </main>
{/if}
{#if top}
  <p class="notice" role="status">
    {word('code', top.code)}
    {#if top.undo}<button onclick={() => (send(top.undo!), seen(top.n))}>{t('undo')}</button>{/if}
    <button aria-label={t('close')} onclick={() => seen(top.n)}>×</button>
  </p>
{/if}
