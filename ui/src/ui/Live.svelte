<script lang="ts">
  import type { Session } from '$proto/Session';
  import type { State } from '$proto/State';
  import { send, store } from '../lib/core.svelte';
  import { clock, t, word } from '../lib/words.svelte';
  import Captions from './Captions.svelte';
  import Room from './Room.svelte';

  let { s, session }: { s: State; session: Session } = $props();
  let now = $state(Date.now());
  $effect(() => {
    const id = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(id);
  });
  const status = $derived(session.status);
  const paused = $derived(status.is === 'Paused');
  const dead = $derived(store.live.some((c) => c.health.is === 'Dead'));
  const live = $derived(s.rooms.find((r) => r.recording === null));
  // After the stop, the recording itself is where the person lands: its id is read first.
  const stop = (id = session.id) => void send({ do: 'Stop' }).then(() => send({ do: 'Open', id }));
</script>

<p class="bar">
  <span class="dot" class:taking={!paused}></span>
  <b class="elapsed">{clock(Math.max(0, now - session.started))}</b>
  <span role="status">{t(`status.${status.is}`)}{status.is === 'AudioOnly' ? ` · ${word('code', status.why)}` : ''}</span>
  {#if dead}<button onclick={() => send({ do: 'Reconnect' })}>{t('reconnect')}</button>{/if}
  <button onclick={() => send({ do: paused ? 'Resume' : 'Pause' })}>{t(paused ? 'resume' : 'pause')}</button>
  <button class="primary" onclick={() => stop()}>{t('stop')}</button>
  <button aria-pressed={s.captions} onclick={() => send({ do: 'Captions', show: !s.captions })}>{t('captions')}</button>
  {#if !live}<button onclick={() => send({ do: 'Share', recording: null })}>{t('audience')}</button>{/if}
</p>
{#if live}<Room room={live} />{/if}
<Captions columns={store.live} layout="Reading" />
