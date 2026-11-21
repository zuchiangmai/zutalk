<script lang="ts">
  import { send, store } from '../lib/core.svelte';
  import { t } from '../lib/words.svelte';
  import Captions from './Captions.svelte';

  // The floating window: dragged by anywhere that is not a control. Its button says the layout.
  const s = $derived(store.state);
  const mode = $derived(s?.setup.mode ?? 'Audience');
  const turn = () => s && send({ do: 'Setup', setup: { ...s.setup, mode: mode === 'Audience' ? 'Conversation' : 'Audience' } });
</script>

<main class="float" data-tauri-drag-region>
  <Captions columns={store.live} layout={mode} />
  <nav>
    <button onclick={turn}>{t(`mode.${mode}`)}</button>
    <button aria-label={t('close')} onclick={() => send({ do: 'Captions', show: false })}>×</button>
  </nav>
</main>
