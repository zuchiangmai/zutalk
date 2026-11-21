<script lang="ts">
  import { store } from '../lib/core.svelte';
  import { room } from '../lib/room.svelte';
  import { clock, langName, t, ui, when, word } from '../lib/words.svelte';
  import Captions from './Captions.svelte';

  // A page for whoever has the link: live captions, or a recording someone shared. It only reads.
  const end = $derived(store.notices.at(-1)?.code); // the room's end, or why it cannot be read
  const open = $derived(room.open);
  const mine = $derived(ui.lang.split('-')[0]);
  let only = $state(''); // one language, picked on a small screen
  const columns = $derived(store.live.filter((c) => !only || c.language === only));
</script>

<header><b>ZuTalk</b>{#if end}<span class="flag" role="status">{word('code', end)}</span>{/if}</header>
<main>
  {#if open}
    <h1 class="title">{open.row.title || when(open.row.started)}</h1>
    <ol class="transcript">
      {#each open.lines as [k, l] (k)}
        <li><small>{open.speakers[l.speaker] || (l.speaker && t('speaker', l.speaker))} {clock(l.t0)}</small>
          <div><p lang={l.language}>{l.text}</p>{#if l.translations[mine] && l.language !== mine}<p class="tr" lang={mine}>{l.translations[mine]}</p>{/if}</div></li>
      {/each}
    </ol>
  {:else if !end}
    {#if store.live.length > 1}
      <p>{#each store.live as c (c.language)}
        <button aria-pressed={only === c.language} onclick={() => (only = only === c.language ? '' : c.language)}>{langName(c.language, c.language)}</button>
      {/each}</p>
    {/if}
    <Captions {columns} layout="Audience" />
  {/if}
</main>
