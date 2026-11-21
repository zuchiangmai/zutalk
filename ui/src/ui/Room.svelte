<script lang="ts">
  import type { Room } from '$proto/Room';
  import { renderSVG } from 'uqr';
  import { say, send } from '../lib/core.svelte';
  import { t } from '../lib/words.svelte';
  import Sure from './Sure.svelte';

  // A room others can open: its link as a code to scan and as text to copy, and how to end it.
  let { room }: { room: Room } = $props();
  const copy = () => navigator.clipboard.writeText(room.link).then(() => say('link'));
</script>

<div class="room">
  <figure class="qr" role="img" aria-label={t('qr')}>{@html renderSVG(room.link, { border: 1 })}</figure>
  <input readonly value={room.link} aria-label={t('link')} onfocus={(e) => e.currentTarget.select()} />
  <button onclick={copy}>{t('link.copy')}</button>
  <Sure label={t('act.unshare')} act={() => send({ do: 'Unshare', room: room.id })} />
</div>
