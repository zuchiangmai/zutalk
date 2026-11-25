<script lang="ts">
  import type { Change } from '$proto/Change';
  import type { RecordingView } from '$proto/RecordingView';
  import type { State } from '$proto/State';
  import { pick, say, send, store } from '../lib/core.svelte';
  import { clock, langName, t, text, word } from '../lib/words.svelte';
  import City from './City.svelte';
  import Meta from './Meta.svelte';
  import Room from './Room.svelte';
  import Sure from './Sure.svelte';

  // A recording: a click on a name or a text corrects it in place; each correction is a record.
  let { s, open }: { s: State; open: RecordingView } = $props();
  const row = $derived(open.row);
  const lines = $derived(store.openLines);
  const change = (change: Change) => send({ do: 'Change', id: row.id, change });
  const langs = $derived([...new Set(lines.flatMap(([, l]) => Object.keys(l.translations)))]);
  let hidden = $state<string[]>([]); // translations put away while reading
  const shown = $derived(langs.filter((l) => !hidden.includes(l)));
  const hits = $derived(new Set(store.hits.filter((h) => h.recording_id === row.id).map((h) => h.key)));
  const copy = () => navigator.clipboard.writeText(text({ lines, speakers: open.speakers }, shown)).then(() => say('copied'));
  const remove = () => (send({ do: 'Remove', id: row.id }), say('remove', { do: 'Restore', id: row.id }));
  // Written out in the languages being read; subtitles in the first of them.
  const write = (exporter: string) =>
    pick(row.title || 'ZuTalk').then((to) => void (to && send({ do: 'Export', id: row.id, exporter, languages: shown, to })));
  const shared = $derived(s.rooms.find((r) => r.recording === row.id));
  const taking = $derived(store.session?.id === row.id); // neither shared nor removed while it is taken
  let editing = $state(''), draft = $state('');
  const edit = (id: string, body: string) => ((editing = id), (draft = body));
  function key(e: KeyboardEvent, was: string, commit: (text: string) => void) {
    if (e.isComposing || e.keyCode === 229) return; // a word still being composed is not an Enter
    if (e.key === 'Escape') editing = '';
    if (e.key !== 'Enter' || e.shiftKey) return;
    e.preventDefault();
    if (draft.trim() && draft.trim() !== was) commit(draft.trim());
    editing = '';
  }
</script>

{#snippet field(id: string, body: string, commit: (text: string) => void, via = '', lang?: string)}
  {#if editing === id}
    <textarea {lang} bind:value={draft} {@attach (el) => el.focus()} onblur={() => (editing = '')} onkeydown={(e) => key(e, body, commit)}></textarea>
  {:else}
    <span class="text" class:unclear={via === 'unclear'} {lang} role="button" tabindex="0" title={t('correct')}
      onclick={() => edit(id, body)} onkeydown={(e) => e.key === 'Enter' && edit(id, body)}>{body}</span>
  {/if}
  {#if via === 'hand' || via === 'unclear'}<small class:flag={via === 'unclear'}>{t(via === 'hand' ? 'via.hand' : 'via.unclear')}</small>{/if}
{/snippet}

<input class="title" value={row.title} placeholder={row.preview || t('untitled')} aria-label={t('title')}
  onchange={(e) => change({ what: 'Title', text: e.currentTarget.value.trim() })} />
<Meta {row} />
<p>
  <button class="primary" onclick={copy}>{t('act.copy')}</button>
  {#each langs as l (l)}
    <button aria-pressed={!hidden.includes(l)} onclick={() => (hidden = hidden.includes(l) ? hidden.filter((x) => x !== l) : [...hidden, l])}>{langName(l)}</button>
  {/each}
  <select aria-label={t('act.export')} value="" onchange={(e) => (write(e.currentTarget.value), (e.currentTarget.value = ''))}>
    <option value="" disabled>{t('act.export')}</option>
    {#each s.exporters as x (x)}<option value={x}>{word('format', x)}</option>{/each}
  </select>
  {#if !taking}
    {#if !shared}<button onclick={() => send({ do: 'Share', recording: row.id })}>{t('act.share')}</button>{/if}
    {#if row.audio}<Sure label={t('act.destroy')} tip={t('destroy.tip')} act={() => send({ do: 'DestroyAudio', id: row.id })} />{/if}
    <button class="danger" onclick={remove}>{t('act.remove')}</button>
  {/if}
</p>
{#if shared}<Room room={shared} />{/if}
{#if !taking}<City {open} />{/if}
<ol class="transcript">
  {#each lines as [k, l] (k)}
    <li class:hit={hits.has(k)}>
      <small>
        {#if l.speaker}{@render field(`who ${k}`, open.speakers[l.speaker] || t('speaker', l.speaker), (name) => change({ what: 'Speaker', label: l.speaker, name }))}{/if}
        {clock(l.t0)}
      </small>
      <div>
        <p>{@render field(`${k} ${l.language}`, l.text, (text) => change({ what: 'Line', key: k, language: l.language, text }), l.via.text, l.language)}
          {#if l.incomplete}<small class="flag">{t('via.incomplete')}</small>{/if}</p>
        {#each shown.filter((x) => x !== l.language && x in l.translations) as x (x)}
          <p class="tr">{@render field(`${k} ${x}`, l.translations[x], (text) => change({ what: 'Line', key: k, language: x, text }), l.via[x], x)}</p>
        {/each}
      </div>
    </li>
  {:else}
    <li class="quiet">{t('notranscript')}</li>
  {/each}
</ol>
<h2><label for="note">{t('notes')}</label></h2>
<textarea id="note" value={open.note} onblur={(e) => e.currentTarget.value !== open.note && change({ what: 'Note', text: e.currentTarget.value })}></textarea>
