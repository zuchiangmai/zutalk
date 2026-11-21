<script lang="ts">
  import type { State } from '$proto/State';
  import { renderSVG } from 'uqr';
  import { send } from '../lib/core.svelte';
  import { type Key, LANGS, langName, t, ui } from '../lib/words.svelte';

  let { s }: { s: State } = $props();
  const money = (micros: number) => new Intl.NumberFormat(ui.lang, { style: 'currency', currency: 'USD' }).format(micros / 1e6);
  const KEEP = [null, 7, 0]; // null: until destroyed; days from its start, once transcribed
  const keep = (k: number | null) => (k === null ? t('keep.always') : k ? t('keep.days', k) : t('keep.written'));
  const langs = $derived([...new Set([...s.setup.spoken, ...s.setup.shown])]); // where a term is written
  function add(e: SubmitEvent) {
    e.preventDefault();
    const form = e.currentTarget as HTMLFormElement;
    const [said, ...forms] = [...form.querySelectorAll('input')].map((i): [string, string] => [i.name, i.value.trim()]);
    if (said[1]) void send({ do: 'Term', said: said[1], written: Object.fromEntries(forms.filter(([, v]) => v)) });
    form.reset();
  }
  function own(e: SubmitEvent, service: string) {
    e.preventDefault();
    const input = (e.currentTarget as HTMLFormElement).elements[0] as HTMLInputElement;
    if (input.value.trim()) void send({ do: 'OwnKey', service, key: input.value.trim() });
    input.value = ''; // what was typed goes to the core and nowhere else
  }
</script>

<h2>{t('s.identity')}</h2>
{#if s.me}
  <p>{s.me} <button onclick={() => send({ do: 'SignOut' })}>{t('signout')}</button></p>
{:else if s.login}
  <figure class="qr" title={t('signin.tip')}>{@html renderSVG(s.login, { border: 1 })}<figcaption>{s.login}</figcaption></figure>
{:else}
  <button onclick={() => send({ do: 'SignIn' })}>{t('signin')}</button>
{/if}
{#if s.rotation_unsaved}
  <!-- The key itself goes from the core to the clipboard; this page never holds it. -->
  <p title={t('recovery.tip')}>
    <span class="quiet">{t('recovery')}</span>
    <button onclick={() => send({ do: 'CopyRotation' })}>{t('act.copy')}</button>
    <button onclick={() => send({ do: 'RotationSaved' })}>{t('recovery.saved')}</button>
  </p>
{/if}
<h2>{t('s.keys')}</h2>
{#each s.keys as [service, from] (service)}
  {@const name = t(`key.${service}` as Key)}
  <form onsubmit={(e) => own(e, service)}>
    <span class="quiet">{name}</span>
    {t(`from.${from}`)}{from === 'Granted' && s.left !== null ? ` · ${t('left', money(s.left))}` : ''}
    {#if from === 'Own'}
      <button type="button" onclick={() => send({ do: 'OwnKey', service, key: null })}>{t('remove')}</button>
    {:else}
      <input type="password" autocomplete="off" aria-label={name} placeholder={name} /><button>{t('save')}</button>
    {/if}
  </form>
{/each}
<h2><label for="keep">{t('audio')}</label></h2>
<select id="keep" title={t('keep.tip')} value={s.setup.keep ?? ''} onchange={(e) => send({ do: 'Setup', setup: { ...s.setup, keep: e.currentTarget.value ? Number(e.currentTarget.value) : null } })}>
  {#each KEEP as k (k)}<option value={k ?? ''}>{keep(k)}</option>{/each}
</select>
<h2>{t('s.terms')}</h2>
<form onsubmit={add} title={t('terms.tip')}>
  {#each s.terms as term (term.said)}
    <button type="button" class="on" aria-label={t('remove.one', term.said)} onclick={() => send({ do: 'Term', said: term.said, written: null })}>{[term.said, ...Object.values(term.written)].join(' · ')} ×</button>
  {/each}
  <input name="said" placeholder={t('term')} aria-label={t('term')} />
  {#each langs as l (l)}<input name={l} placeholder={langName(l)} aria-label={langName(l)} />{/each}
  <button>{t('add')}</button>
</form>
<h2><label for="lang">{t('s.language')}</label></h2>
<select id="lang" value={s.setup.language ?? ''} onchange={(e) => send({ do: 'Setup', setup: { ...s.setup, language: e.currentTarget.value || null } })}>
  <option value="">{t('lang.system')}</option>
  {#each LANGS as l (l)}<option value={l}>{langName(l, l)}</option>{/each}
</select>
